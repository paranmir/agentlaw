//! Pure command-line grammar. Parsing this module never opens application resources.

use clap::{
    builder::{PossibleValue, StringValueParser, TypedValueParser},
    Args, Parser, Subcommand, ValueEnum,
};
use std::{ffi::OsStr, path::PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "agentlaw",
    version,
    about = "Persistent memory and work continuity for your agent",
    disable_help_subcommand = true,
    infer_long_args = false,
    infer_subcommands = false,
    args_override_self = false,
    after_help = "Examples:\n  agentlaw help sync resolve\n  agentlaw describe share import resolve\n\nFor values beginning with '-', use --option=VALUE, for example --value=--help.\nUse '--' to end option parsing for positional values."
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Show contextual help for a command path.
    Help(HelpArgs),
    /// Describe the public command grammar as JSON.
    Describe(DescribeArgs),
    /// Print the unchanged Agentlaw tool schema.
    Schema,
    /// Execute one Agentlaw JSON request from stdin.
    #[command(
        after_help = "Example:\n  agentlaw call --json -\n\nSupply one complete Agentlaw JSON request on stdin; progress is written to stderr."
    )]
    Call(CallArgs),
    /// Run the MCP transport.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Update the managed installation, or inspect an exact update plan.
    #[command(
        after_help = "Examples:\n  agentlaw update\n  agentlaw update check\n  agentlaw update status <returned-plan-id>\n\nUse the installed stable agentlaw command for a complete synchronous update: download, verification, replacement, and planned cleanup finish before an ordinary harness restart is requested. An installed result confirms installation completion; it does not prove the running harness has reloaded or semantic recall is ready."
    )]
    Update(UpdateArgs),
    /// Optional support actions.
    Support {
        #[command(subcommand)]
        command: SupportCommand,
    },
    /// Propose or confirm installation into a supported harness.
    #[command(
        after_help = "Example:\n  agentlaw install --harness codex --harness-dir <absolute-profile>\n\nThis proposes targets. Repeat with --confirm-install only after the user approves those targets."
    )]
    Install(InstallArgs),
    /// Diagnose the selected installation and memory store.
    Doctor,
    /// Recover recorded publication and rebuild derived generations.
    Repair,
    /// Inspect and change delivery settings.
    #[command(
        after_help = "Examples:\n  agentlaw config get history.response_limit_bytes\n  agentlaw config set response_limit_bytes 65536\n\nHistory requires a positive byte count; general delivery requires at least 4096 bytes. Changes apply to the next request."
    )]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Inspect or name this machine identity.
    Machine {
        #[command(subcommand)]
        command: MachineCommand,
    },
    /// Export complete memory history.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Propose, create, or connect a memory store.
    #[command(
        after_help = "Examples:\n  agentlaw store propose-location\n  agentlaw store connect --path <absolute-existing-store>\n\nCreate a store only after the user confirms its location; inspect store create --help for that command."
    )]
    Store {
        #[command(subcommand)]
        command: StoreCommand,
    },
    /// List or search learned procedures.
    LearnedProcedure {
        #[command(subcommand)]
        command: LearnedProcedureCommand,
    },
    /// Save an explicit continuity snapshot.
    Continuity {
        #[command(subcommand)]
        command: ContinuityCommand,
    },
    /// Manage fixed-cutoff synchronization and its local delegation policy.
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
    /// Inspect sharing, fetch incoming work, or review an isolated import.
    Share {
        #[command(subcommand)]
        command: ShareCommand,
    },
    #[command(hide = true)]
    WorkerDaemon(WorkerDaemonArgs),
    #[command(hide = true)]
    ModelChild,
}

#[derive(Debug, Args)]
pub(crate) struct HelpArgs {
    /// Public command path, for example: share import resolve.
    #[arg(value_name = "COMMAND", num_args = 0..)]
    pub path: Vec<String>,
    /// Output format for contextual help.
    #[arg(long, value_enum)]
    pub format: Option<HelpFormat>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum HelpFormat {
    Text,
    Json,
}

#[derive(Debug, Args)]
pub(crate) struct DescribeArgs {
    /// Public command path; omit to describe the root.
    #[arg(value_name = "COMMAND", num_args = 0..)]
    pub path: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct CallArgs {
    /// Read one JSON request from stdin; the supported source is '-'.
    #[arg(long, value_name = "-", value_parser = ["-"])]
    pub json: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    /// Serve the Agentlaw MCP tool over standard input and output.
    Serve(McpServeArgs),
}

#[derive(Debug, Args)]
pub(crate) struct McpServeArgs {
    /// Use the standard-input/output transport.
    #[arg(long, required = true)]
    pub stdio: bool,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct UpdateArgs {
    // Stable launcher compatibility wire; not a public user workflow.
    #[arg(long, value_name = "PLAN_ID", hide = true)]
    pub confirm_update: Option<String>,
    #[command(subcommand)]
    pub command: Option<UpdateCommand>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum UpdateCommand {
    /// Check the public release endpoint for a newer full release.
    Check,
    /// Inspect the exact returned update plan.
    #[command(
        after_help = "Example:\n  agentlaw update status <returned-plan-id>\n\nUse the known plan ID returned by an update. Omit --root to use the selected installation, or provide its absolute managed root."
    )]
    Status(UpdateStatusArgs),
    #[command(hide = true)]
    Apply(UpdateApplyArgs),
}

#[derive(Debug, Args)]
pub(crate) struct UpdateStatusArgs {
    /// Exact plan ID returned by an update operation.
    #[arg(value_name = "PLAN_ID")]
    pub plan_id: String,
    /// Explicit managed installation root.
    #[arg(long, value_name = "PATH")]
    pub root: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct UpdateApplyArgs {
    #[arg(value_name = "PLAN_ID")]
    pub plan_id: String,
    #[arg(long, value_name = "PATH")]
    pub root: PathBuf,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SupportCommand {
    /// Ask whether to star Agentlaw with the authenticated GitHub account.
    Star(SupportStarArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SupportStarArgs {
    /// Reopen a choice previously declined by this account.
    #[arg(long)]
    pub ask_again: bool,
}

#[derive(Debug, Args)]
pub(crate) struct InstallArgs {
    /// Harness adapter to configure.
    #[arg(long, value_parser = HarnessNameParser)]
    pub harness: HarnessName,
    /// Target harness profile directory; defaults to the adapter's profile.
    #[arg(long, value_name = "PATH")]
    pub harness_dir: Option<PathBuf>,
    /// Local model artifact manifest.
    #[arg(long, value_name = "PATH")]
    pub model_manifest: Option<PathBuf>,
    /// Confirm the proposed installation; omit for a proposal only.
    #[arg(long)]
    pub confirm_install: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum HarnessName {
    Codex,
    OhMyPi,
}

impl HarnessName {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::OhMyPi => "oh-my-pi",
        }
    }
}

/// Keep the install domain's public failure code while retaining clap's enum
/// metadata and handling every other argv condition in the same grammar.
#[derive(Clone)]
struct HarnessNameParser;

impl TypedValueParser for HarnessNameParser {
    type Value = HarnessName;

    fn parse_ref(
        &self,
        command: &clap::Command,
        argument: Option<&clap::Arg>,
        value: &OsStr,
    ) -> std::result::Result<Self::Value, clap::Error> {
        StringValueParser::new()
            .try_map(|name: String| {
                agentlaw_app::install::Harness::parse(&name).map(|harness| match harness {
                    agentlaw_app::install::Harness::Codex => HarnessName::Codex,
                    agentlaw_app::install::Harness::OhMyPi => HarnessName::OhMyPi,
                })
            })
            .parse_ref(command, argument, value)
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        Some(Box::new(
            HarnessName::value_variants()
                .iter()
                .filter_map(ValueEnum::to_possible_value),
        ))
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Print the selected installation's configuration path.
    Path,
    /// Read a delivery setting.
    Get(ConfigGetArgs),
    /// Change a delivery setting after domain validation.
    Set(ConfigSetArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ConfigGetArgs {
    #[arg(value_enum)]
    pub key: ConfigKey,
}

#[derive(Debug, Args)]
pub(crate) struct ConfigSetArgs {
    #[arg(value_enum)]
    pub key: ConfigKey,
    /// Byte count; the setting's supported range is checked by configuration.
    #[arg(value_name = "BYTES")]
    pub value: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum ConfigKey {
    #[value(name = "history.response_limit_bytes")]
    HistoryResponseLimitBytes,
    #[value(name = "response_limit_bytes")]
    ResponseLimitBytes,
}

impl ConfigKey {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::HistoryResponseLimitBytes => "history.response_limit_bytes",
            Self::ResponseLimitBytes => "response_limit_bytes",
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum MachineCommand {
    /// Inspect this machine identity.
    Inspect,
    /// Set the user-chosen display name.
    #[command(
        after_help = "Example:\n  agentlaw machine name --value=--help\n\nThe equals form treats an option-like display name as a literal value."
    )]
    Name(MachineNameArgs),
}

#[derive(Debug, Args)]
pub(crate) struct MachineNameArgs {
    #[arg(long, value_name = "DISPLAY_NAME")]
    pub value: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum HistoryCommand {
    /// Export complete versioned memory history to a new file.
    Export(HistoryExportArgs),
}

#[derive(Debug, Args)]
pub(crate) struct HistoryExportArgs {
    #[arg(long, value_name = "UUID")]
    pub memory_id: String,
    #[arg(long, value_name = "PATH")]
    pub output: PathBuf,
}

#[derive(Debug, Subcommand)]
pub(crate) enum StoreCommand {
    /// Propose a memory-store location without creating it.
    ProposeLocation,
    /// Create a memory store at the confirmed location.
    #[command(
        after_help = "After the user confirms the proposed location:\n  agentlaw store create --path <absolute-confirmed-store> --confirm-create"
    )]
    Create(StoreCreateArgs),
    /// Connect an existing canonical memory store.
    Connect(StoreConnectArgs),
}

#[derive(Debug, Args)]
pub(crate) struct StoreCreateArgs {
    #[arg(long, value_name = "PATH")]
    pub path: PathBuf,
    #[arg(long, required = true)]
    pub confirm_create: bool,
}

#[derive(Debug, Args)]
pub(crate) struct StoreConnectArgs {
    #[arg(long, value_name = "PATH")]
    pub path: PathBuf,
}

#[derive(Debug, Subcommand)]
pub(crate) enum LearnedProcedureCommand {
    /// Produce a complete procedure inventory in the selected scopes.
    #[command(
        after_help = "Example:\n  agentlaw learned-procedure list --scope project --project <id-or-hint> --format table\n\nOmit scope filters for all management scopes. --output must name a new derived file outside canonical memory."
    )]
    List(InventoryArgs),
    /// Search procedure descriptors; defaults to five IDs.
    #[command(
        after_help = "Example:\n  agentlaw learned-procedure search --query updates --limit 5 --format jsonl\n\nSearch shows procedure IDs and descriptors, with diagnostics on stderr. List produces complete inventory."
    )]
    Search(ProcedureSearchArgs),
}

#[derive(Debug, Args)]
pub(crate) struct InventoryArgs {
    #[arg(long, value_enum)]
    pub scope: Option<InventoryScope>,
    #[arg(long, value_name = "ID_OR_HINT")]
    pub project: Option<String>,
    #[arg(long, value_name = "ID")]
    pub machine: Option<String>,
    /// Write derived output to a new file outside canonical memory.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "jsonl")]
    pub format: ProcedureFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum InventoryScope {
    User,
    Project,
    Machine,
    #[value(name = "project_machine")]
    ProjectMachine,
}

impl InventoryScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Machine => "machine",
            Self::ProjectMachine => "project_machine",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProcedureFormat {
    Jsonl,
    Table,
}

impl From<InventoryArgs> for agentlaw_app::inventory::Options {
    fn from(args: InventoryArgs) -> Self {
        Self {
            output: args.output,
            scope: args.scope.map(|scope| scope.as_str().to_owned()),
            project: args.project,
            machine: args.machine,
            table: args.format == ProcedureFormat::Table,
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct ProcedureSearchArgs {
    #[arg(long, value_name = "TEXT")]
    pub query: String,
    #[arg(long, value_name = "POSITIVE_INTEGER", value_parser = positive_usize)]
    pub limit: Option<usize>,
    #[command(flatten)]
    pub inventory: InventoryArgs,
}

fn positive_usize(value: &str) -> std::result::Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| "expected a positive integer within the platform's supported range".into())
}

#[derive(Debug, Subcommand)]
pub(crate) enum ContinuityCommand {
    /// Save an explicit Git continuity snapshot.
    Save,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SyncCommand {
    /// Start one fixed-cutoff operation under a registered policy.
    #[command(
        after_help = "Example:\n  agentlaw sync start --policy <registered-policy-id> --request-id <unique-request-id>\n\nUse a policy already reviewed and activated through the separate local delegation workflow."
    )]
    Start(SyncStartArgs),
    /// Inspect a returned operation without supplying a new solution.
    #[command(
        after_help = "Example:\n  agentlaw sync status --operation <returned-operation-id>\n\nCopy the returned operation ID; status accepts no solution or new revision."
    )]
    Status(SyncStatusArgs),
    /// Submit a user-reviewed whole solution for this exact revision.
    #[command(
        after_help = "Example:\n  agentlaw sync resolve --operation <returned-operation-id> --revision <returned-revision> --request-id <unique-request-id> --solution <whole-solution.json>\n\nRead the complete frozen packet and submit the agreed whole solution. Use --solution - to read JSON from stdin."
    )]
    Resolve(SyncResolveArgs),
    /// Resume the exact recorded operation revision.
    Resume(SyncMutationArgs),
    /// Hold the exact recorded operation revision.
    Hold(SyncMutationArgs),
    /// Cancel the exact recorded operation revision.
    Cancel(SyncMutationArgs),
    /// Accept sensitive findings for the exact candidate and destination.
    AcceptFindings(SyncAcceptFindingsArgs),
    /// Propose or explicitly configure OS-local delegation.
    #[command(
        after_help = "Example:\n  agentlaw sync policy propose --remote <name> --target-ref refs/heads/<branch>\n\nThe proposal is disabled and unregistered. Configure only the user-reviewed local policy after separate delegation approval."
    )]
    Policy {
        #[command(subcommand)]
        command: SyncPolicyCommand,
    },
}

#[derive(Debug, Args)]
pub(crate) struct SyncStartArgs {
    #[arg(long, value_name = "ID")]
    pub policy: String,
    #[arg(long, value_name = "UNIQUE_ID")]
    pub request_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct SyncStatusArgs {
    #[arg(long, value_name = "ID")]
    pub operation: String,
}

#[derive(Debug, Args)]
pub(crate) struct SyncMutationArgs {
    #[arg(long, value_name = "ID")]
    pub operation: String,
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub revision: u64,
    #[arg(long, value_name = "UNIQUE_ID")]
    pub request_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct SyncResolveArgs {
    #[command(flatten)]
    pub operation: SyncMutationArgs,
    /// Complete JSON solution file, or '-' for standard input.
    #[arg(long, value_name = "JSON_FILE_OR_STDIN")]
    pub solution: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct SyncAcceptFindingsArgs {
    #[arg(long, value_name = "ID")]
    pub operation: String,
    #[arg(long, value_name = "OID")]
    pub candidate: String,
    #[arg(long, value_name = "DIGEST")]
    pub findings_digest: String,
    #[arg(long)]
    pub confirm_sharing: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SyncPolicyCommand {
    /// Propose a disabled policy for review without registering it.
    Propose(ShareInspectArgs),
    /// Configure a reviewed local policy with separate delegation approval.
    #[command(
        after_help = "After the user approves the reviewed local policy:\n  agentlaw sync policy configure --file <reviewed-policy.json> --confirm-delegation\n\nDelegation approval does not approve sharing sensitive findings."
    )]
    Configure(SyncPolicyConfigureArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SyncPolicyConfigureArgs {
    #[arg(long, value_name = "PATH")]
    pub file: PathBuf,
    #[arg(long)]
    pub confirm_delegation: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ShareCommand {
    /// Inspect an exact outgoing destination and its sensitive findings.
    Inspect(ShareInspectArgs),
    /// Push a reviewed result with a separate decision for flagged content.
    Push(SharePushArgs),
    /// Fetch a configured remote without publishing incoming memory.
    Fetch(ShareFetchArgs),
    /// Review incoming work in an isolated workspace before publication.
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },
}

#[derive(Debug, Args)]
pub(crate) struct ShareInspectArgs {
    #[arg(long, value_name = "NAME")]
    pub remote: String,
    #[arg(long, value_name = "REF")]
    pub target_ref: String,
}

#[derive(Debug, Args)]
pub(crate) struct SharePushArgs {
    #[arg(long, value_name = "RETURNED_REF")]
    pub review: String,
    #[arg(long, requires = "user_confirmed")]
    pub allow_sensitive: bool,
    #[arg(long, requires = "allow_sensitive")]
    pub user_confirmed: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ShareFetchArgs {
    #[arg(long, value_name = "NAME")]
    pub remote: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ImportCommand {
    /// Prepare an isolated review workspace for an immutable commit.
    Prepare(ImportPrepareArgs),
    /// Inspect a returned isolated import.
    Inspect(ImportRefArgs),
    /// Execute an Agentlaw request within an isolated import workspace.
    Call(ImportCallArgs),
    /// Freeze reviewed import state, optionally applying confirmed choices.
    #[command(
        after_help = "Examples:\n  agentlaw share import resolve --ref <returned-import-ref>\n\nAfter the user chooses local/incoming for the returned structural conflicts:\n  agentlaw share import resolve --ref <returned-import-ref> --choices <choices.json> --user-confirmed\n\nStructural choices do not authorize publication of the resolved import."
    )]
    Resolve(ImportResolveArgs),
    /// Publish the exact resolved import after separate user confirmation.
    #[command(
        after_help = "After the user confirms the complete resolved import:\n  agentlaw share import publish --ref <returned-import-ref> --resolution <returned-resolution-token> --user-confirmed"
    )]
    Publish(ImportPublishArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ImportPrepareArgs {
    #[arg(long, value_name = "OID")]
    pub commit: String,
}

#[derive(Debug, Args)]
pub(crate) struct ImportRefArgs {
    #[arg(long = "ref", value_name = "RETURNED_REF")]
    pub import_ref: String,
}

#[derive(Debug, Args)]
pub(crate) struct ImportCallArgs {
    #[arg(long = "ref", value_name = "RETURNED_REF")]
    pub import_ref: String,
    #[command(flatten)]
    pub input: CallArgs,
}

#[derive(Debug, Args)]
pub(crate) struct ImportResolveArgs {
    #[arg(long = "ref", value_name = "RETURNED_REF")]
    pub import_ref: String,
    /// Object mapping returned conflict IDs to local/incoming choices.
    #[arg(long, value_name = "JSON_FILE_OR_STDIN", requires = "user_confirmed")]
    pub choices: Option<PathBuf>,
    #[arg(long, requires = "choices")]
    pub user_confirmed: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ImportPublishArgs {
    #[arg(long = "ref", value_name = "RETURNED_REF")]
    pub import_ref: String,
    #[arg(long, value_name = "RETURNED_TOKEN")]
    pub resolution: String,
    #[arg(long)]
    pub user_confirmed: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkerDaemonArgs {
    #[arg(long, value_name = "PATH")]
    pub state_dir: PathBuf,
    #[arg(long, value_name = "PATH", requires_all = ["tokenizer", "ort_library"])]
    pub model: Option<PathBuf>,
    #[arg(long, value_name = "PATH", requires_all = ["model", "ort_library"])]
    pub tokenizer: Option<PathBuf>,
    #[arg(long, value_name = "PATH", requires_all = ["model", "tokenizer"])]
    pub ort_library: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{error::ErrorKind, CommandFactory};

    fn parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("agentlaw").chain(args.iter().copied()))
    }

    #[test]
    fn command_tree_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn examples_from_command_metadata_parse_without_execution() {
        fn check(command: &clap::Command) -> usize {
            let mut checked = 0;
            if let Some(after_help) = command.get_after_help() {
                for line in after_help.to_string().lines() {
                    let line = line.trim();
                    if !line.starts_with("agentlaw ") {
                        continue;
                    }
                    let args: Vec<_> = line
                        .split_whitespace()
                        .map(|value| {
                            if value == "<returned-revision>" {
                                "1"
                            } else {
                                value
                            }
                        })
                        .collect();
                    assert!(Cli::try_parse_from(args).is_ok(), "invalid example: {line}");
                    checked += 1;
                }
            }
            checked + command.get_subcommands().map(check).sum::<usize>()
        }
        assert!(check(&Cli::command()) >= 10);
    }

    #[test]
    fn root_and_help_paths_are_pure_information_shapes() {
        assert!(parse(&[]).unwrap().command.is_none());
        let Some(Command::Help(args)) = parse(&["help", "share", "import", "resolve"])
            .unwrap()
            .command
        else {
            panic!("help shape changed");
        };
        assert_eq!(args.path, ["share", "import", "resolve"]);
        assert_eq!(args.format, None);
        assert!(matches!(
            parse(&["describe"]).unwrap().command,
            Some(Command::Describe(_))
        ));
    }

    #[test]
    fn contextual_help_bypasses_required_execution_arguments() {
        for args in [
            vec!["install", "--help"],
            vec!["sync", "resolve", "--help"],
            vec!["share", "import", "publish", "-h"],
            vec!["store", "create", "--help"],
            vec!["worker-daemon", "--help"],
        ] {
            assert_eq!(parse(&args).unwrap_err().kind(), ErrorKind::DisplayHelp);
        }
        assert_eq!(
            parse(&["--version"]).unwrap_err().kind(),
            ErrorKind::DisplayVersion
        );
    }

    #[test]
    fn singleton_values_and_flags_reject_duplicates() {
        for args in [
            vec!["machine", "name", "--value", "a", "--value", "b"],
            vec![
                "install",
                "--harness",
                "codex",
                "--confirm-install",
                "--confirm-install",
            ],
            vec!["support", "star", "--ask-again", "--ask-again"],
            vec![
                "learned-procedure",
                "list",
                "--format",
                "jsonl",
                "--format",
                "table",
            ],
            vec!["sync", "status", "--operation", "a", "--operation", "b"],
        ] {
            assert_eq!(
                parse(&args).unwrap_err().kind(),
                ErrorKind::ArgumentConflict,
                "{args:?}"
            );
        }
    }

    #[test]
    fn static_command_shapes_reject_unsupported_combinations() {
        for args in [
            vec!["sync", "status", "--operation", "a", "--solution", "-"],
            vec![
                "sync",
                "start",
                "--policy",
                "p",
                "--request-id",
                "r",
                "--operation",
                "o",
            ],
            vec!["share", "push", "--review", "r", "--allow-sensitive"],
            vec!["share", "push", "--review", "r", "--user-confirmed"],
            vec!["share", "import", "resolve", "--ref", "r", "--choices", "-"],
            vec![
                "share",
                "import",
                "resolve",
                "--ref",
                "r",
                "--user-confirmed",
            ],
            vec!["worker-daemon", "--state-dir", "state", "--model", "model"],
            vec!["update", "--confirm-update", "p", "check"],
            vec!["update", "status"],
            vec!["mcp", "serve"],
            vec!["store", "create", "--path", "store"],
            vec!["schema", "--unknown"],
            vec!["sche"],
            vec!["machine", "name", "--val", "a"],
        ] {
            assert!(parse(&args).is_err(), "unexpectedly accepted {args:?}");
        }
    }

    #[test]
    fn update_preserves_every_existing_wire_shape() {
        assert!(matches!(
            parse(&["update"]).unwrap().command,
            Some(Command::Update(UpdateArgs {
                command: None,
                confirm_update: None
            }))
        ));
        for args in [
            vec!["update", "check"],
            vec!["update", "--confirm-update", "p"],
            vec!["update", "status", "p"],
            vec!["update", "status", "p", "--root", "root"],
            vec!["update", "apply", "p", "--root", "root"],
        ] {
            assert!(parse(&args).is_ok(), "wire rejected {args:?}");
        }
    }

    #[test]
    fn literal_help_value_and_option_terminator_are_respected() {
        let Some(Command::Machine {
            command: MachineCommand::Name(args),
        }) = parse(&["machine", "name", "--value=--help"])
            .unwrap()
            .command
        else {
            panic!("machine name shape changed");
        };
        assert_eq!(args.value, "--help");
        let Some(Command::Update(UpdateArgs {
            command: Some(UpdateCommand::Status(args)),
            ..
        })) = parse(&["update", "status", "--", "--help"])
            .unwrap()
            .command
        else {
            panic!("update status shape changed");
        };
        assert_eq!(args.plan_id, "--help");
        // Pinned clap resolves a missing option value before the next help flag.
        assert_eq!(
            parse(&["machine", "name", "--value", "--help"])
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidValue
        );
    }

    #[test]
    fn runtime_paths_with_spaces_are_preserved() {
        let args = [
            std::ffi::OsString::from("agentlaw"),
            std::ffi::OsString::from("store"),
            std::ffi::OsString::from("connect"),
            std::ffi::OsString::from("--path"),
            std::ffi::OsString::from("memory directory"),
        ];
        let Some(Command::Store {
            command: StoreCommand::Connect(args),
        }) = Cli::try_parse_from(args).unwrap().command
        else {
            panic!("store connect shape changed");
        };
        assert_eq!(args.path, PathBuf::from("memory directory"));
    }

    #[test]
    fn sync_revision_range_matches_the_canonical_input_contract() {
        for command in ["resume", "hold", "cancel", "resolve"] {
            let mut args = vec![
                "sync",
                command,
                "--operation",
                "OP",
                "--revision",
                "0",
                "--request-id",
                "REQ",
            ];
            if command == "resolve" {
                args.extend(["--solution", "-"]);
            }
            assert_eq!(parse(&args).unwrap_err().kind(), ErrorKind::ValueValidation);
        }
        for command in ["resume", "resolve"] {
            let mut request = serde_json::json!({"action":"sync","sync":{"command":command,"operation_id":"OP","expected_revision":1,"request_id":"REQ"}});
            if command == "resolve" {
                request["sync"]["solution"] = serde_json::json!({"units":[],"redirects":[],"projects":[],"dependent_dispositions":[]});
            }
            assert!(agentlaw_contracts::parse_request(&request.to_string()).is_ok());
            request["sync"]["expected_revision"] = serde_json::json!(0);
            assert_eq!(
                agentlaw_contracts::parse_request(&request.to_string())
                    .unwrap_err()
                    .code,
                "invalid_input"
            );
        }
    }

    #[cfg(any(unix, windows))]
    fn assert_os_path_roundtrip(path: std::ffi::OsString) {
        assert!(path.to_str().is_none(), "fixture must not be valid Unicode");
        let args = [
            std::ffi::OsString::from("agentlaw"),
            std::ffi::OsString::from("store"),
            std::ffi::OsString::from("connect"),
            std::ffi::OsString::from("--path"),
            path.clone(),
        ];
        let Some(Command::Store {
            command: StoreCommand::Connect(args),
        }) = Cli::try_parse_from(args).unwrap().command
        else {
            panic!("store connect shape changed");
        };
        assert_eq!(args.path.into_os_string(), path);
    }

    #[cfg(unix)]
    #[test]
    fn unix_non_utf8_paths_roundtrip_without_lossy_decoding() {
        use std::os::unix::ffi::OsStringExt;
        assert_os_path_roundtrip(std::ffi::OsString::from_vec(b"memory-\xff".to_vec()));
    }

    #[cfg(windows)]
    #[test]
    fn windows_unpaired_surrogate_paths_roundtrip_without_lossy_decoding() {
        use std::os::windows::ffi::OsStringExt;
        assert_os_path_roundtrip(std::ffi::OsString::from_wide(&[
            0x006d, 0x0065, 0x006d, 0xd800,
        ]));
    }
}
