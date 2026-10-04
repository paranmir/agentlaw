//! Pure argv parsing and information requests; no application resources here.

use super::{
    commands::{Cli, Command, HelpFormat, LearnedProcedureCommand},
    help,
};
use agentlaw_contracts::{DomainError, Result};
use clap::{
    error::{ContextKind, ContextValue, ErrorKind},
    CommandFactory, Parser,
};
use std::{error::Error as _, ffi::OsString};

#[derive(Debug)]
pub(crate) enum Parsed {
    Information(String),
    Execute(Command),
}

/// `args` excludes the executable. Paths stay as OS strings until clap's typed
/// path parser consumes them. Never fall back to another parser after rejection.
pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Parsed> {
    let args: Vec<_> = args.into_iter().collect();
    let cli = match Cli::try_parse_from(
        std::iter::once(OsString::from("agentlaw")).chain(args.iter().cloned()),
    ) {
        Ok(cli) => cli,
        Err(error) => {
            return match error.kind() {
                ErrorKind::DisplayHelp => {
                    help::display_help(&error.to_string()).map(Parsed::Information)
                }
                ErrorKind::DisplayVersion => {
                    Ok(Parsed::Information(error.to_string().trim_end().to_owned()))
                }
                _ => Err(argument_error(&error, &args)),
            };
        }
    };
    match cli.command {
        None => Ok(Parsed::Information(help::root_json())),
        Some(Command::Schema) => Ok(Parsed::Information(agentlaw_app::schema().to_string())),
        Some(Command::Help(args)) => {
            let information = match (args.path.is_empty(), args.format) {
                (true, None | Some(HelpFormat::Json)) => Ok(help::root_json()),
                (_, None | Some(HelpFormat::Text)) => help::text(&args.path),
                (false, Some(HelpFormat::Json)) => help::describe(&args.path),
            }?;
            Ok(Parsed::Information(information))
        }
        Some(Command::Describe(args)) => help::describe(&args.path).map(Parsed::Information),
        Some(command) => {
            validate(&command)?;
            Ok(Parsed::Execute(command))
        }
    }
}

fn argument_error(error: &clap::Error, args: &[OsString]) -> DomainError {
    let path = help::valid_command_prefix(args);
    // clap retains typed value-parser failures as sources. Preserve our domain
    // error, never clap's diagnostic text (which can echo a supplied value).
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(domain) = cause.downcast_ref::<DomainError>() {
            return with_help(domain.clone(), &path);
        }
        source = cause.source();
    }
    if missing_install_harness(error, &path) {
        return with_help(
            DomainError::new(
                "harness_required",
                "Specify --harness codex or --harness oh-my-pi.",
            ),
            &path,
        );
    }
    if matches!(
        error.kind(),
        ErrorKind::InvalidValue | ErrorKind::ValueValidation
    ) && path.iter().map(String::as_str).eq(["config", "set"])
    {
        return with_help(
            DomainError::new(
                "invalid_configuration",
                "Use a supported delivery setting and an integer byte count.",
            ),
            &path,
        );
    }
    help::invalid_arguments(error_message(error.kind()), &path)
}

fn with_help(mut error: DomainError, path: &[String]) -> DomainError {
    error.message = help::invalid_arguments(&error.message, path).message;
    error
}

fn missing_install_harness(error: &clap::Error, path: &[String]) -> bool {
    if error.kind() != ErrorKind::MissingRequiredArgument
        || !path.iter().map(String::as_str).eq(["install"])
    {
        return false;
    }
    let Some(ContextValue::Strings(missing)) = error.get(ContextKind::InvalidArg) else {
        return false;
    };
    let mut command = Cli::command();
    command.build();
    command
        .find_subcommand("install")
        .and_then(|install| {
            install
                .get_arguments()
                .find(|argument| argument.get_id().as_str() == "harness")
        })
        .is_some_and(|harness| missing.len() == 1 && missing[0] == harness.to_string())
}

fn error_message(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::UnknownArgument => "Unknown argument.",
        ErrorKind::InvalidSubcommand => "Unknown command.",
        ErrorKind::ArgumentConflict => "Duplicate or incompatible arguments.",
        ErrorKind::MissingRequiredArgument => "A required argument is missing.",
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            "A command is required."
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => "An argument value is invalid.",
        ErrorKind::NoEquals
        | ErrorKind::TooManyValues
        | ErrorKind::TooFewValues
        | ErrorKind::WrongNumberOfValues => "An argument has an invalid number or form of values.",
        _ => "Invalid command-line arguments.",
    }
}

fn validate(command: &Command) -> Result<()> {
    if let Command::LearnedProcedure {
        command: LearnedProcedureCommand::Search(args),
    } = command
    {
        if args.query.trim().is_empty() {
            return Err(help::invalid_arguments(
                "The procedure query must not be empty.",
                &["learned-procedure".into(), "search".into()],
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::commands::{MachineCommand, UpdateArgs, UpdateCommand};
    use super::*;
    use serde_json::Value;

    fn argv(args: &[&str]) -> Result<Parsed> {
        parse(args.iter().map(|value| OsString::from(*value)))
    }

    fn information(args: &[&str]) -> String {
        let Parsed::Information(value) = argv(args).unwrap() else {
            panic!("information request became execution");
        };
        value
    }

    #[test]
    fn root_help_aliases_are_json_and_text_is_explicit() {
        let expected = information(&[]);
        for args in [
            &["--help"][..],
            &["-h"],
            &["help"],
            &["help", "--format", "json"],
        ] {
            assert_eq!(information(args), expected);
        }
        let root: Value = serde_json::from_str(&expected).unwrap();
        assert_eq!(root["status"], "help");
        assert!(information(&["help", "--format", "text"]).contains("Usage: agentlaw"));
    }

    #[test]
    fn contextual_help_does_not_require_execution_arguments() {
        let named = information(&["help", "sync", "resolve"]);
        assert_eq!(information(&["sync", "resolve", "--help"]), named);
        assert!(information(&["store", "create", "-h"]).contains("--confirm-create"));
        assert!(information(&["install", "--help"]).contains("--harness"));
    }

    #[test]
    fn help_literals_and_terminators_are_execution_values() {
        let Parsed::Execute(Command::Machine {
            command: MachineCommand::Name(args),
        }) = argv(&["machine", "name", "--value=--help"]).unwrap()
        else {
            panic!("literal value became help");
        };
        assert_eq!(args.value, "--help");
        let Parsed::Execute(Command::Update(UpdateArgs {
            command: Some(UpdateCommand::Status(args)),
            ..
        })) = argv(&["update", "status", "--", "--help"]).unwrap()
        else {
            panic!("terminated positional became help");
        };
        assert_eq!(args.plan_id, "--help");
    }

    #[test]
    fn invalid_paths_and_arguments_are_sanitized_and_targeted() {
        for args in [
            vec!["sync", "resolve", "--unknown-secret-token"],
            vec![
                "machine",
                "name",
                "--value",
                "private-value",
                "--value",
                "another-private-value",
            ],
            vec!["help", "share", "private-path"],
            vec!["describe", "worker-daemon"],
            vec!["worker-daemon", "--help"],
        ] {
            let error = argv(&args).unwrap_err();
            assert_eq!(error.code, "invalid_arguments");
            assert!(!error.message.contains("private"));
            assert!(!error.message.contains("secret"));
            assert!(!error.message.contains("Usage:"));
        }
        assert!(argv(&["sync", "resolve", "--bad"])
            .unwrap_err()
            .message
            .contains("agentlaw sync resolve --help"));
        assert!(argv(&["help", "share", "unknown"])
            .unwrap_err()
            .message
            .contains("agentlaw share --help"));
    }

    #[test]
    fn whitespace_query_is_rejected_before_execution() {
        assert_eq!(
            argv(&["learned-procedure", "search", "--query", "  "])
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }

    #[test]
    fn schema_stays_the_mcp_schema_and_describe_is_selected_cli_metadata() {
        assert_eq!(information(&["schema"]), agentlaw_app::schema().to_string());
        let selected: Value =
            serde_json::from_str(&information(&["describe", "share", "import", "resolve"]))
                .unwrap();
        assert_eq!(
            selected["path"],
            serde_json::json!(["share", "import", "resolve"])
        );
        assert_eq!(
            information(&["help", "share", "import", "resolve", "--format", "json"]),
            information(&["describe", "share", "import", "resolve"])
        );
    }
}
