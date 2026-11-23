//! Typed sharing and isolated-import execution.
use super::{
    commands::{ImportCommand, ShareCommand},
    general::runtime_admin,
    input::{import_choices, read_stdin},
};
use agentlaw_contracts::Result;
use agentlaw_runtime::protocol::AdminCommand;
use serde_json::Value;

pub(super) fn execute(command: ShareCommand) -> Result<Value> {
    let command = match command {
        ShareCommand::Inspect(args) => AdminCommand::ShareInspect {
            remote: args.remote,
            target_ref: args.target_ref,
        },
        ShareCommand::Push(args) => AdminCommand::SharePush {
            review: args.review,
            allow_sensitive: args.allow_sensitive,
            user_confirmed: args.user_confirmed,
        },
        ShareCommand::Fetch(args) => AdminCommand::ShareFetch {
            remote: args.remote,
        },
        ShareCommand::Import { command } => match command {
            ImportCommand::Prepare(args) => AdminCommand::ImportPrepare {
                commit: args.commit,
            },
            ImportCommand::List => AdminCommand::ImportList,
            ImportCommand::Inspect(args) => AdminCommand::ImportInspect {
                import_ref: args.import_ref,
            },
            ImportCommand::Call(args) => AdminCommand::ImportCall {
                import_ref: args.import_ref,
                request: agentlaw_contracts::validation::decode_unique(&read_stdin()?)?,
            },
            ImportCommand::Resolve(args) => AdminCommand::ImportResolve {
                import_ref: args.import_ref,
                choices: args
                    .choices
                    .as_deref()
                    .map(import_choices)
                    .transpose()?
                    .map(agentlaw_contracts::to_value)
                    .transpose()?,
                user_confirmed: args.user_confirmed,
            },
            ImportCommand::Publish(args) => AdminCommand::ImportPublish {
                import_ref: args.import_ref,
                resolution: args.resolution,
                user_confirmed: args.user_confirmed,
            },
        },
    };
    runtime_admin(command)
}
