# Agentlaw usage

Agentlaw attaches to an existing agent harness. One MCP tool, `agentlaw`, provides
recall, deliberate memory updates, history and learned procedures. The CLI uses
the same input contract. Runtime does not replace the agent's semantic judgment
or the user's decisions.

For installation and harness setup, see the [installation guide](installation.md).

## CLI help and input

No arguments, `--help`, `-h`, and bare `help` return root JSON with `name`, a
`commands` array of generated usage strings, and `status: "help"`. Use
`agentlaw help --format text` for root text help. A command's `--help`/`-h` and
`agentlaw help <command-path>` show its options and examples without requiring
its execution arguments:

```text
agentlaw sync resolve --help
agentlaw help share import resolve
agentlaw describe share import resolve
agentlaw help sync resolve --format json
```

`describe` emits selected public CLI metadata: `name`, `path`, `usage`, nullable
`about`, `options`, and `commands`. Each option includes `name`, nullable
`long`/`short`, `required`, `takes_value`, and `possible_values`; each child
command includes `name` and nullable `about`. Omit the describe path for root
metadata. This is separate from the unchanged MCP schema returned by `schema`.
Root help keeps its JSON shape but changes the old implementation status to
`help`, generates its command strings, and omits internal commands. These are
explicit content changes, not a promise of byte-identical old help.

Option order is flexible. Duplicate singleton execution options or confirmation
flags, unknown options/abbreviations, and unsupported combinations are rejected.
Use `--value=--help` for an option-like literal value; `--` ends option parsing
for positional values. Recognized help/version requests bypass required execution
arguments, but do not override a missing value for a preceding option:
`machine name --value --help` is invalid.

Help and argv rejected by pure parsing do not read stdin/payload files or open configuration,
stores, workers, or network connections. An older installed stable launcher
still performs its existing read-only managed-root/layout-marker bootstrap.
Call/import-call JSON, sync solution/policy JSON, and structural-choice JSON use
only their explicit stdin/file source after valid argv, bounded to 16 MiB plus
one byte to detect overflow. They pass UTF-8/JSON and existing domain validation
before application resources open. Installation model manifests/assets and
export files keep their existing limits. CLI syntax checks do not replace domain
validation. Existing result/error streams remain intact; procedure inventory/search
still stream JSONL or tables and diagnostics.

## Isolated setup and installation

Set `AGENTLAW_HOME` to an absolute installation-local state directory for a
development trial. Keep it separate from the active canonical memory store.
Without this override, an installed executable uses its managed layout marker;
an unmanaged executable does not silently adopt older AppData state. The default
Windows installer root is the current user's `Agentlaw` directory, derived from
the user profile rather than a hard-coded account name.
The release installers use a managed root outside AppData instead:
`<selected-root>/bin`, `<selected-root>/state`, `<selected-root>/memory` and
`<selected-root>/models`. Its executable discovers `state` using the layout
marker when `AGENTLAW_HOME` is unset; the harness adapter explicitly configures
that same state path. `store propose-location` returns the sibling memory path,
but does not create it. LLVM-MinGW/Rust build tools are for source development,
not required by the release and not installed automatically.
The Windows installer refuses a new root when another managed root is found;
one OS user shares one installation across harnesses. Reuse the
same `-RootDir` to upgrade a nondefault Windows root. The Unix installer uses
`$HOME/Agentlaw` by default, accepts `AGENTLAW_ROOT` for another complete root,
and refuses unrecognized content at the selected root.
`AGENTLAW_HOME` continues to override the installation state directory. The
`source-coordination` registry lives under that installation's `state` directory.
All clients use this registry so the same Markdown source cannot bind to
independent local recovery directories. The app explicitly passes the registry
to storage, diagnostics, repair, and Git staging. Isolated tests use isolated
registries, not a profile-global registry.
The registry is local state, separate from Git-shared memory.
Index generation acknowledgements include their absolute backend directory.
After an explicit offline relocation has updated source/control/recovery bindings
and harness paths, run `agentlaw repair` from the new installation to rebuild
derived generations. Copying index files alone is not sufficient. Verify a real
semantic recall and subsequent write/index advancement before retiring backups.
`repair` is not a relocation command and does not rewrite those source bindings.
These path conventions are not evidence of cross-platform testing.

```text
agentlaw store propose-location
agentlaw store create --path <confirmed-absolute-path> --confirm-create
agentlaw machine name --value <user-chosen-name>
agentlaw store connect --path <existing-canonical-store>
```

Creation requires confirmation. Connection validates source and prepares indexes
before selecting it. With a configured model, semantic preparation must finish;
without model assets, semantic unavailability is explicit. Interrupted index
construction resumes. Failed A-to-B switching leaves A selected; success applies
to existing frontends on their **next request**. In-flight requests and retained
proposals stay with their original binding.

Installation is a separate explicit operation:

```text
agentlaw install --harness codex --harness-dir <isolated-profile>
agentlaw install --harness codex --harness-dir <confirmed-profile> --model-manifest <manifest.json> --confirm-install
```

Without `--confirm-install`, this only proposes targets. Confirmation installs a
versioned executable and verified assets, updates owned MCP/bootstrap entries,
preserves unrelated settings and records recoverable progress. Codex Desktop/CLI
share the Codex adapter. `--harness oh-my-pi` has a configuration adapter; live
harness verification is deferred. Omit `--harness-dir` only when deliberately
targeting the active user profile.

The model manifest contains three **absolute local paths** and SHA-256 digests:

```json
{
  "model_build_id": "granite-r2-256-portable-qdq-v1",
  "onnx_model": {"path": "<absolute-model.onnx>", "sha256": "<64-hex-digest>"},
  "tokenizer_json": {"path": "<absolute-tokenizer.json>", "sha256": "<64-hex-digest>"},
  "runtime_library": {"path": "<absolute-platform-ORT-library>", "sha256": "<64-hex-digest>"}
}
```

[ARTIFACTS.md](../crates/agentlaw-worker/ARTIFACTS.md) records the portable Granite
QDQ build and reproducible build-only recipe. Product startup does not quantize
or silently download an alternative model. A development frontend can instead
use `AGENTLAW_ONNX_MODEL`, `AGENTLAW_TOKENIZER` and `AGENTLAW_ORT_LIBRARY` together.
Missing inference is never replaced with fake vectors.

## Managed updates and optional GitHub support

An ordinary MCP result may include `update_notice` when Agentlaw has verified a
newer full GitHub release. The release check is bounded and runs outside memory
work. A failed check does not claim that the installation is current. Ordinary
MCP work does not inspect historical update plans; the update command reports
its own success or exact blocker using the plan it is already executing. A
running MCP process keeps its loaded version until the harness restarts.

For an installed, managed root:

```text
agentlaw update check
agentlaw update
agentlaw update status <plan-id>
```

`update check` queries the public release endpoint explicitly. The public
`agentlaw` command in `command/` owns a complete synchronous update. It pins the
latest full release and the owned harness registrations, verifies the archive
and candidate, gates new Agentlaw work, and waits for affected MCP and worker
processes to finish and exit. It replaces the bundle and registrations, probes
the new MCP with initialize, tool discovery and read-only recall, then removes
only the old bundle, previous registered version and staging objects approved
by that plan. Memory, models, machine identity and pending work are preserved.
On exit 0 with valid final `status: "installed"`, report completion and request
a normal harness restart. The updater already verified its exact plan and
finished replacement, probe, cleanup, and gate release. Do not repeat
check/status/version/schema/doctor/recall solely to prove installation again.
Installation does not establish semantic readiness or that the current harness
has loaded the new runtime. `up_to_date` is a version result, not installation
verification; an internal `handoff_ready` or completed plan alone is not the
public `installed` result.

An incomplete result names its blocker and never requests a restart as if
installation succeeded. If the final result is missing, inconsistent, or failed,
inspect the known plan ID with `update status <plan-id>`, or recover the same plan
from its owned durable marker before retrying the stable command. An unknown
plan identity is not permission to blindly create another plan. The installed
runtime checks unfinished plans on the explicit update command before checking
for a newer release; ordinary MCP memory calls do not scan them.

The versioned runtime's bare `update` remains a preview for compatibility with
already released stable launchers. Their prepare/apply/status wire remains
accepted; this CLI change does not automatically replace those launchers.
Prepare/apply entry points are hidden from public help, not protected by that
hiding. Public `update status` requires a known plan ID; no planless status or
new internal alias is added. Use the installed stable public command for the
complete update. A source build using `AGENTLAW_HOME` for an installed state
cannot approve a managed update.

`agentlaw support star` is a separate optional action. With an authenticated
GitHub CLI account, it checks whether that account already starred
`paranmir/agentlaw`; only a verified unstarred account at an interactive terminal
gets a `y/N` prompt. `n` is remembered for that account; `--ask-again` reopens
the choice. Star approval never approves an update, and an update never stars
the repository.

## Ordinary memory work

`mcp serve --stdio` exposes only `agentlaw`. `call --json -` reads one JSON request
from stdin and emits one result on stdout; progress goes to stderr. Use `schema`
for the current contract. Do not fabricate IDs or observed versions.

```json
{"action":"recall","recall":{"recall_for":"What context should inform this work?"}}
```

For project work the agent supplies the actual folder observed through its
harness. Neither the MCP working directory nor the executable directory identifies
that project. One discovered project candidate alone does not authorize binding.

Recall combines scope-filtered discovery with exact rules, Task context and
required references. Selected bodies remain complete. An indivisible result that
exceeds delivery limits is provided as a frozen complete file with **not-yet-read**
state and access/follow-up instructions. File creation does not imply the model
read it. Artifacts report expiry; their cleanup excludes source and pending work.

```text
agentlaw config path
agentlaw config get history.response_limit_bytes
agentlaw config set history.response_limit_bytes 8192
agentlaw config set response_limit_bytes 65536
agentlaw doctor
agentlaw repair
```

History defaults to 8192 response bytes; general delivery defaults to 65536.
These control delivery, not silent truncation. `doctor` diagnoses without source
repair. `repair` completes recorded publication and rebuilds derived generations.
`doctor --audit`, `repair`, `continuity save`, `history export`, and
`learned-procedure list` and `search` run inside the installation runtime, starting
it if needed. During repair, other requests to the selected store wait until it finishes.
Default `doctor` does not start the runtime: `worker_runtime` reports `not_running`, with
`stale_endpoint` when an old address remains. An unresponsive runtime reports
`worker_diagnosis_incomplete` and `runtime_unresponsive`. A running runtime
reports component states and unresolved reasons in `worker_runtime.diagnostics`,
including `model_load` and `vector_index:<binding>`. Resolved failures have no
diagnostic history. The top-level status still reports source and local integrity.
A damaged journal can be rebuilt from retained authoritative decisions while
preserving originals. A missing/corrupt proposal DB needs backup recovery, never
a successful-looking empty replacement.

Recall candidates group identical clues from the same source into one
`retrieval_paths` entry. Its `via` array lists every actual discovery channel:

```json
{"via":["lexical","vector"],"clue":"Use argument arrays for paths with spaces."}
```

Different clues or link-source IDs remain separate. Active Task candidates and
complete-content files use the same representation. Builds with this change
emit an array for `via`; releases through v0.3.5 emitted a string. Consumers
reading that response field must account for the new shape.

## Git and management

### Fixed-cutoff sync (0.4.0)

This operation is introduced in 0.4.0. An older installed runtime does not gain
the new tool action until a separately authorized update and harness restart.

The agent starts one operation, submits a whole solution only if conflicts require
judgment, then reports completion. Agentlaw owns mechanical Git operations and
recovery. Ordinary `remember_this` still creates no Git commit or push.

Activate a continuing local delegation separately and only at the user's request:

```text
agentlaw sync policy propose --remote <name> --target-ref <refs/heads/name>
agentlaw sync policy configure --file <reviewed-policy.json> --confirm-delegation
```

`propose` returns an `enabled:false` JSON policy, without registering it. Save a
reviewed copy, explicitly set `enabled:true`, then configure it. Review its actual
source/control paths, fetch/push endpoints, target ref, allowed operations/scopes
and push permission. Updating/revoking the same policy changes its version/digest;
unstarted effects stop, while decided canonical recovery and mandatory Git handoff
still finish. This OS-local policy is not a signed per-call approval or a security
boundary against malicious tools executing as the same user. LLM calls cannot
create, edit, enable or broaden policies.

```text
agentlaw sync start --policy <registered-id> --request-id <unique-id>
agentlaw sync status --operation <returned-id>
agentlaw sync resolve --operation <id> --revision <returned> --request-id <unique-id> --solution <whole-solution.json-or->
agentlaw sync resume --operation <id> --revision <returned> --request-id <unique-id>
agentlaw sync hold --operation <id> --revision <returned> --request-id <unique-id>
agentlaw sync cancel --operation <id> --revision <returned> --request-id <unique-id>
```

The MCP/CLI `call` shape is `{"action":"sync","sync":{...}}`. Use the current
schema for typed solution fields. Read the complete frozen packet, including
base/local/incoming bodies, metadata, evidence and dependent dispositions.
Large packets are complete immutable JSON files with path/digest/bytes and an
explicit unread marker, not truncations. Copy returned handles and revisions;
Runtime allocates change/version identities and checks both branches' lineage,
required references and the final graph. Rejected replacements close the gate.
For response loss, replay the same request ID and payload before introducing
a new request. Status/recovery checks completed effects before obsolete inputs.

`completed_for_cutoff` means the fixed candidate was locally applied, handed off
to Git and confirmed at the target. Later local saves remain available, unstaged
relative to that commit, for a future explicit sync. It does not mean a clean or
globally latest working tree. Local-only overlay conflicts keep the candidate and
scan receipt. Remote advancement permits one new candidate from the old candidate,
not the latest local tail; a second advancement pauses rather than rewriting history.
Exact existing tree/parent coverage reuses the commit instead of adding empty Git
history. A missing required predecision view fails closed without creating empty
memory; a completed status remains independent of old stage files. With push
permission disabled, `local_completed_push_not_delegated` is partial local success,
not remote completion; it does not implicitly broaden the policy.

Sensitive findings still require a separate exact-candidate user decision:

```text
agentlaw sync accept-findings --operation <id> --candidate <OID> --findings-digest <returned> --confirm-sharing
```

`--confirm-sharing` never activates delegation, and `--confirm-delegation` never
approves sensitive findings. Acceptance binds candidate, findings, scanner,
policy and destination. Sharing unchanged or cancelling is supported; redaction,
exclusion or history rewrite needs a separate scoped decision, not a bypass.
Scanning materializes a self-contained raw Git transfer store. Retries validate
its integrity and reuse pattern findings; new candidates are scanned again.
Replace refs, grafts, shallow/partial/promisor stores, alternates and gitlinks are
unsupported, without silently reconfiguring Git. Capture/projection is limited
to 100,000 canonical files and 512 MiB, with a cooperative 5-second capture
budget. Final guarded context is limited to 256 nodes/paths and 16 MiB; large
connected graphs may be refused. Convenience snapshots and requests retain
64 MiB and 16 MiB limits respectively. The capture budget is not a hard OS I/O
deadline, and these bounds are not a measured large-corpus performance guarantee.

### Existing separate continuity/import/share commands

```text
agentlaw continuity save
agentlaw share inspect --remote <name> --target-ref <refs/heads/name>
agentlaw share push --review <returned-review>
agentlaw share fetch --remote <name>
agentlaw share import prepare --commit <immutable-OID>
agentlaw share import inspect --ref <returned-import>
agentlaw share import call --ref <returned-import> --json -
agentlaw share import resolve --ref <returned-import>
agentlaw share import resolve --ref <returned-import> --choices <file-or-> --user-confirmed
agentlaw share import publish --ref <returned-import> --resolution <returned-token> --user-confirmed
```

Import editing stays isolated until the exact final result is confirmed.
Conflicting heads use ordinary `evolve`/`consolidate` in that workspace. Structural
choices map returned conflict IDs to `local` or `incoming`. A structural choice
is not approval to publish. Source publication and Git handoff are separately
recoverable.

Outgoing Git history is scanned for identifiable secret patterns. Flagged content
needs an explicit user decision; private hosting is not approval. The flags
`--allow-sensitive --user-confirmed` authorize only the reviewed push. They do not
authorize redacting canonical memory or rewriting Git history.

Management uses `learned-procedure list` and `learned-procedure search --query
<text>`; `--help` lists filters and output formats. `history export --memory-id
<id> --output <new-file>` writes complete versioned JSONL through a temporary
file, without overwriting existing output.
