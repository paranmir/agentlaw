//! Typed CLI input adaptation; the sync domain owns validation and execution.

use super::commands::{SyncCommand, SyncMutationArgs, SyncPolicyCommand};
use agentlaw_app::{config, machine, sync};
use agentlaw_contracts::{DomainError, Request, Result};
use serde_json::{json, Value};
use std::path::Path;

pub(super) fn execute(command: SyncCommand) -> Result<Value> {
    let input = match command {
        SyncCommand::Start(args) => {
            json!({"command":"start","policy_id":args.policy,"request_id":args.request_id})
        }
        SyncCommand::Status(args) => {
            json!({"command":"status","operation_id":args.operation})
        }
        SyncCommand::Resolve(args) => {
            let text =
                super::input::read_path_or_stdin(&args.solution).map_err(|error| {
                    match error.code.as_str() {
                        "transport_capacity" => DomainError::new(
                            "request_too_large",
                            "Sync solution exceeds the 16 MiB request limit.",
                        ),
                        "input_unreadable" | "transport_io" | "invalid_encoding" => {
                            let stage = if args.solution == Path::new("-") {
                                "sync solution stdin"
                            } else {
                                "sync solution file"
                            };
                            DomainError::new(
                                "git_io",
                                format!("Git {stage} failed; local memory was preserved."),
                            )
                        }
                        _ => error,
                    }
                })?;
            let solution = agentlaw_contracts::validation::decode_unique(&text)?;
            let mut input = mutation_input("resolve", args.operation);
            input["solution"] = solution;
            input
        }
        SyncCommand::Resume(args) => mutation_input("resume", args),
        SyncCommand::Hold(args) => mutation_input("hold", args),
        SyncCommand::Cancel(args) => mutation_input("cancel", args),
        SyncCommand::AcceptFindings(args) => {
            sync::require_sharing_confirmation(args.confirm_sharing)?;
            let (store, local) = super::dispatch::selected_store()?;
            return sync::accept_findings(
                &store,
                &local,
                &config::state_root()?,
                &args.operation,
                &args.candidate,
                &args.findings_digest,
                args.confirm_sharing,
            );
        }
        SyncCommand::Policy { command } => return execute_policy(command),
    };
    // Reuse the MCP/domain input contract before resolving any installation,
    // selected store or machine identity.
    let request =
        agentlaw_contracts::parse_request(&json!({"action":"sync","sync":input}).to_string())?;
    let Request::Sync(request) = request else {
        return Err(DomainError::new(
            "invalid_input",
            "Expected a sync request.",
        ));
    };
    let (store, local) = super::dispatch::selected_store()?;
    let state = config::state_root()?;
    let selection = config::load(&state)?.ok_or_else(|| {
        DomainError::new(
            "memory_store_connection_required",
            "Connect a memory store before syncing.",
        )
    })?;
    let identity = machine::load_or_create(&state)?;
    sync::call(
        &store,
        &local,
        &state,
        &selection.user_id,
        &identity.machine_id,
        request,
        super::dispatch::cli_control(),
    )
}

fn mutation_input(command: &str, args: SyncMutationArgs) -> Value {
    json!({
        "command":command,
        "operation_id":args.operation,
        "expected_revision":args.revision,
        "request_id":args.request_id
    })
}

fn execute_policy(command: SyncPolicyCommand) -> Result<Value> {
    match command {
        SyncPolicyCommand::Propose(args) => {
            let (store, local) = super::dispatch::selected_store()?;
            agentlaw_contracts::to_value(sync::propose_policy(
                &store,
                &local,
                &args.remote,
                &args.target_ref,
            )?)
        }
        SyncPolicyCommand::Configure(args) => {
            sync::require_delegation_confirmation(args.confirm_delegation)?;
            let text = super::input::read_file(&args.file).map_err(|error| {
                let stage = match error.code.as_str() {
                    "input_unreadable" => "sync record read",
                    "invalid_encoding" => "sync record decode",
                    _ => return error,
                };
                DomainError::new(
                    "git_io",
                    format!("Git {stage} failed; local memory was preserved."),
                )
            })?;
            let policy = sync::prepare_policy_configuration(&text, args.confirm_delegation)?;
            let (store, local) = super::dispatch::selected_store()?;
            sync::configure_prepared_policy(&config::state_root()?, &store, &local, policy)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::commands::{
        SyncAcceptFindingsArgs, SyncPolicyConfigureArgs, SyncResolveArgs,
    };
    use super::*;

    fn mutation() -> SyncMutationArgs {
        SyncMutationArgs {
            operation: "invalid-operation".into(),
            revision: 1,
            request_id: "request".into(),
        }
    }

    #[test]
    fn validates_sync_requests_before_installation_resources() {
        let invalid_revision = || SyncMutationArgs {
            revision: 0,
            ..mutation()
        };
        for command in [
            SyncCommand::Resume(invalid_revision()),
            SyncCommand::Hold(invalid_revision()),
            SyncCommand::Cancel(invalid_revision()),
        ] {
            assert_eq!(execute(command).unwrap_err().code, "invalid_input");
        }
    }

    #[test]
    fn requires_confirmation_before_policy_file_or_resources() {
        let command = SyncCommand::Policy {
            command: SyncPolicyCommand::Configure(SyncPolicyConfigureArgs {
                file: "not-a-readable-policy.json".into(),
                confirm_delegation: false,
            }),
        };
        assert_eq!(
            execute(command).unwrap_err().code,
            "delegation_confirmation_required"
        );
        assert_eq!(
            execute(SyncCommand::AcceptFindings(SyncAcceptFindingsArgs {
                operation: "operation".into(),
                candidate: "candidate".into(),
                findings_digest: "digest".into(),
                confirm_sharing: false,
            }))
            .unwrap_err()
            .code,
            "sharing_choice_required"
        );
    }

    #[test]
    fn local_confirmation_flags_cannot_grant_a_different_authority() {
        // The typed argv boundary now rejects the other command's confirmation
        // before a policy file, store, or machine can be opened.
        for args in [
            vec![
                "sync",
                "policy",
                "configure",
                "--file",
                "not-a-readable-policy.json",
                "--confirm-sharing",
            ],
            vec![
                "sync",
                "accept-findings",
                "--operation",
                "unused",
                "--candidate",
                "unused",
                "--findings-digest",
                "unused",
                "--confirm-delegation",
            ],
        ] {
            assert_eq!(
                super::super::parse::parse(args.into_iter().map(std::ffi::OsString::from))
                    .unwrap_err()
                    .code,
                "invalid_arguments"
            );
        }
    }

    #[test]
    fn rejects_duplicate_solution_keys_before_resources() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), r#"{"units":[],"units":[]}"#).unwrap();
        assert_eq!(
            execute(SyncCommand::Resolve(SyncResolveArgs {
                operation: mutation(),
                solution: file.path().into(),
            }))
            .unwrap_err()
            .code,
            "invalid_input"
        );
    }

    #[test]
    fn validates_policy_payload_before_resources() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "{}").unwrap();
        assert_eq!(
            execute(SyncCommand::Policy {
                command: SyncPolicyCommand::Configure(SyncPolicyConfigureArgs {
                    file: file.path().into(),
                    confirm_delegation: true,
                }),
            })
            .unwrap_err()
            .code,
            "git_io"
        );
    }

    #[test]
    fn preserves_solution_file_read_error_before_resources() {
        let directory = tempfile::tempdir().unwrap();
        let invalid_encoding = directory.path().join("invalid-encoding.json");
        std::fs::write(&invalid_encoding, [0xff]).unwrap();
        for path in [directory.path().join("missing.json"), invalid_encoding] {
            assert_eq!(
                execute(SyncCommand::Resolve(SyncResolveArgs {
                    operation: mutation(),
                    solution: path,
                }))
                .unwrap_err()
                .code,
                "git_io"
            );
        }
    }

    #[test]
    fn preserves_oversized_solution_error_before_resources() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file()
            .set_len((agentlaw_app::MAX_REQUEST_BYTES + 1) as u64)
            .unwrap();
        assert_eq!(
            execute(SyncCommand::Resolve(SyncResolveArgs {
                operation: mutation(),
                solution: file.path().into(),
            }))
            .unwrap_err()
            .code,
            "request_too_large"
        );
    }
}
