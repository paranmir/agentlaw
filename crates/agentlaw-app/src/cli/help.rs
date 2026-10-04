//! Information generated from the same public grammar that parses execution.

use super::commands::Cli;
use agentlaw_contracts::{DomainError, Result};
use clap::{Command, CommandFactory};
use serde_json::{json, Value};
use std::ffi::OsString;

fn tree() -> Command {
    let mut command = Cli::command();
    command.build();
    command
}

pub(super) fn root_json() -> String {
    let mut usages = Vec::new();
    for command in tree()
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
    {
        collect_usages(command, &mut usages);
    }
    json!({"name": "agentlaw", "status": "help", "commands": usages}).to_string()
}

fn collect_usages(command: &Command, usages: &mut Vec<String>) {
    let children: Vec<_> = command
        .get_subcommands()
        .filter(|child| !child.is_hide_set())
        .collect();
    // Optional subcommands (notably bare runtime `update`) also have a valid
    // parent invocation. Required-subcommand parents only contribute leaves.
    if children.is_empty() || !command.is_subcommand_required_set() {
        let usage = usage(command);
        usages.push(usage.strip_prefix("agentlaw ").unwrap_or(&usage).to_owned());
    }
    for child in children {
        collect_usages(child, usages);
    }
}

fn usage(command: &Command) -> String {
    command
        .clone()
        .render_usage()
        .to_string()
        .trim()
        .strip_prefix("Usage: ")
        .expect("clap usage prefix")
        .to_owned()
}

fn resolve(path: &[String]) -> Result<(Command, Vec<String>)> {
    let root = tree();
    let mut command = &root;
    let mut canonical = Vec::new();
    for part in path {
        command = command
            .find_subcommand(part)
            .filter(|child| !child.is_hide_set())
            .ok_or_else(|| invalid_arguments("Unknown public command path.", &canonical))?;
        canonical.push(command.get_name().to_owned());
    }
    Ok((command.clone(), canonical))
}

pub(super) fn text(path: &[String]) -> Result<String> {
    let (mut command, _) = resolve(path)?;
    Ok(command.render_long_help().to_string().trim_end().to_owned())
}

pub(super) fn describe(path: &[String]) -> Result<String> {
    let (command, canonical) = resolve(path)?;
    let options: Vec<Value> = command
        .get_arguments()
        .filter(|argument| !argument.is_hide_set())
        .map(|argument| {
            let possible_values: Vec<String> = argument
                .get_possible_values()
                .into_iter()
                .filter(|value| !value.is_hide_set())
                .map(|value| value.get_name().to_owned())
                .collect();
            json!({
                "name": argument.get_id().as_str(),
                "long": argument.get_long(),
                "short": argument.get_short(),
                "required": argument.is_required_set(),
                "takes_value": argument.get_action().takes_values(),
                "possible_values": possible_values,
            })
        })
        .collect();
    let commands: Vec<Value> = command
        .get_subcommands()
        .filter(|child| !child.is_hide_set())
        .map(|child| {
            json!({
                "name": child.get_name(),
                "about": child.get_about().map(ToString::to_string),
            })
        })
        .collect();
    Ok(json!({
        "name": "agentlaw",
        "path": canonical,
        "usage": usage(&command),
        "about": command.get_about().map(ToString::to_string),
        "options": options,
        "commands": commands,
    })
    .to_string())
}

/// Match actual clap help control flow to a public node. This deliberately
/// never interprets an argv value as a help flag, nor exposes hidden protocols.
pub(super) fn display_help(rendered: &str) -> Result<String> {
    let mut root = tree();
    if is_help_for(&mut root, rendered) {
        return Ok(root_json());
    }
    if let Some(help) = root
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
        .find_map(|command| find_help(command, rendered))
    {
        return Ok(help);
    }
    Err(invalid_arguments("Unknown public help path.", &[]))
}

fn is_help_for(command: &mut Command, rendered: &str) -> bool {
    command.render_help().to_string().trim_end() == rendered.trim_end()
        || command.render_long_help().to_string().trim_end() == rendered.trim_end()
}

fn find_help(command: &Command, rendered: &str) -> Option<String> {
    let mut command = command.clone();
    if is_help_for(&mut command, rendered) {
        return Some(command.render_long_help().to_string().trim_end().to_owned());
    }
    let help = command
        .get_subcommands()
        .filter(|child| !child.is_hide_set())
        .find_map(|child| find_help(child, rendered));
    help
}

/// Only grammar-owned command names reach an error hint. Values and unknown
/// tokens are never echoed, and options stop this prefix-only lookup.
pub(super) fn valid_command_prefix(args: &[OsString]) -> Vec<String> {
    let root = tree();
    let mut command = &root;
    let mut path = Vec::new();
    for token in args {
        let Some(child) = command
            .find_subcommand(token)
            .filter(|child| !child.is_hide_set())
        else {
            break;
        };
        path.push(child.get_name().to_owned());
        command = child;
    }
    path
}

pub(super) fn invalid_arguments(message: &str, path: &[String]) -> DomainError {
    let command = if path.is_empty() {
        "agentlaw".to_owned()
    } else {
        format!("agentlaw {}", path.join(" "))
    };
    DomainError::new(
        "invalid_arguments",
        format!("{message} Use '{command} --help' for help."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_information_excludes_hidden_protocols() {
        let root: Value = serde_json::from_str(&root_json()).unwrap();
        assert_eq!(root["status"], "help");
        assert!(root["commands"]
            .as_array()
            .unwrap()
            .iter()
            .all(Value::is_string));
        let information = format!(
            "{}{}{}",
            root_json(),
            text(&[]).unwrap(),
            describe(&[]).unwrap()
        );
        for hidden in ["worker-daemon", "model-child", "confirm-update"] {
            assert!(!information.contains(hidden), "exposed {hidden}");
        }
        let update = describe(&["update".into()]).unwrap();
        assert!(!update.contains("confirm-update"));
        let update: Value = serde_json::from_str(&update).unwrap();
        assert!(update["commands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|child| child["name"] != "apply"));
    }

    #[test]
    fn describe_retains_argument_types_and_required_flags() {
        let install: Value = serde_json::from_str(&describe(&["install".into()]).unwrap()).unwrap();
        let harness = install["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|option| option["name"] == "harness")
            .unwrap();
        assert_eq!(harness["long"], "harness");
        assert_eq!(harness["required"], true);
        assert_eq!(harness["takes_value"], true);
        assert_eq!(harness["possible_values"], json!(["codex", "oh-my-pi"]));
    }
}
