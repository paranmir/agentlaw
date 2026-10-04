mod cli;

use agentlaw_contracts::DomainError;
use std::ffi::{OsStr, OsString};

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let errors = ErrorPolicy::for_args(&args);
    let parsed = match cli::parse(args) {
        Ok(parsed) => parsed,
        Err(error) => errors.report(&error),
    };
    match parsed {
        cli::Parsed::Information(information) => println!("{information}"),
        cli::Parsed::Execute(command) => {
            let errors = ErrorPolicy::for_command(&command);
            match cli::execute(command) {
                Ok(Some(value)) => println!("{value}"),
                Ok(None) => {}
                Err(error) => errors.report(&error),
            }
        }
    }
}

/// Original argv only chooses legacy reporting for a rejected command;
/// successfully parsed transports select their policy from the typed command.
#[derive(Debug, PartialEq, Eq)]
enum ErrorPolicy {
    Stdout,
    Stderr,
    Worker,
    ModelChild,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_commands_keep_legacy_error_streams() {
        for (args, expected) in [
            (vec!["mcp", "serve", "--stdio"], ErrorPolicy::Stderr),
            (vec!["mcp", "serve"], ErrorPolicy::Stdout),
            (
                vec!["mcp", "serve", "--stdio", "extra"],
                ErrorPolicy::Stdout,
            ),
            (vec!["model-child"], ErrorPolicy::ModelChild),
            (vec!["model-child", "extra"], ErrorPolicy::Stdout),
            (vec!["worker-daemon", "--bad"], ErrorPolicy::Worker),
            (
                vec!["learned-procedure", "search", "--bad"],
                ErrorPolicy::Stderr,
            ),
            (
                vec!["learned-procedure", "list", "--bad"],
                ErrorPolicy::Stderr,
            ),
            (vec!["learned-procedure", "unknown"], ErrorPolicy::Stdout),
        ] {
            let args: Vec<_> = args.into_iter().map(OsString::from).collect();
            assert_eq!(ErrorPolicy::for_args(&args), expected);
        }
    }
}

impl ErrorPolicy {
    fn for_args(args: &[OsString]) -> Self {
        let first = args.first().map(OsString::as_os_str);
        let second = args.get(1).map(OsString::as_os_str);
        if args.len() == 1 && first == Some(OsStr::new("model-child")) {
            return Self::ModelChild;
        }
        if args.len() == 3
            && first == Some(OsStr::new("mcp"))
            && second == Some(OsStr::new("serve"))
            && args[2] == OsStr::new("--stdio")
        {
            return Self::Stderr;
        }
        match (first, second) {
            (Some(first), _) if first == OsStr::new("worker-daemon") => Self::Worker,
            (Some(first), Some(second))
                if first == OsStr::new("learned-procedure")
                    && (second == OsStr::new("list") || second == OsStr::new("search")) =>
            {
                Self::Stderr
            }
            _ => Self::Stdout,
        }
    }

    fn for_command(command: &cli::Command) -> Self {
        match command {
            cli::Command::ModelChild => Self::ModelChild,
            cli::Command::WorkerDaemon(_) => Self::Worker,
            cli::Command::Mcp { .. } | cli::Command::LearnedProcedure { .. } => Self::Stderr,
            _ => Self::Stdout,
        }
    }

    fn report(self, error: &DomainError) -> ! {
        let code = match self {
            Self::Stdout => {
                println!("{}", agentlaw_app::error_payload(error));
                agentlaw_app::exit_code(error)
            }
            Self::Stderr => {
                eprintln!("{}", agentlaw_app::error_payload(error));
                agentlaw_app::exit_code(error)
            }
            Self::Worker => {
                eprintln!("{}", agentlaw_app::error_payload(error));
                1
            }
            Self::ModelChild => {
                eprintln!("agentlaw: private model child failed; consult the broker diagnostic");
                1
            }
        };
        std::process::exit(code)
    }
}
