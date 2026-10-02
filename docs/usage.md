# Agentlaw usage

Agentlaw attaches to an existing agent harness. One MCP tool, `agentlaw`, provides
recall, deliberate memory updates, history and learned procedures. The CLI uses
the same input contract. Runtime does not replace the agent's semantic judgment
or the user's decisions.

For installation and harness setup, see the [installation guide](installation.md).
For tested behavior and current gaps, see [verification](verification.md).

## Modules and source of truth

| Crate | Responsibility |
|---|---|
| agentlaw-contracts | Shared types, schema, validation and LLM-facing guidance |
| agentlaw-flows | Project context, recall, write/review, procedures and history |
| agentlaw-storage | Complete current Markdown, lossless history, publication/recovery |
| agentlaw-search | BM25, vector candidates, RRF, scope filters and generations |
| agentlaw-worker | Shared broker, model lifecycle, durable jobs and index acknowledgment |
| agentlaw-app | MCP/CLI, explicit installation, configuration and Git operations |

The [README](../README.md#how-the-pieces-fit) introduces the user-facing pieces.
Current memory is directly readable Markdown: reading it does not replay every
past change. History retains causal changes separately. Indexes and embeddings
are rebuildable; unpublished proposals and recovery decisions are not disposable
caches. Git synchronization is explicit, not a condition of memory-write success.

## Build

```powershell
cargo check --workspace
cargo build --workspace
.\target\debug\agentlaw.exe --help
.\target\debug\agentlaw.exe schema
```

The development Windows machine uses Rust/Cargo 1.98.1 and LLVM-MinGW on the user
PATH, target `stable-x86_64-pc-windows-gnullvm`. Ignored `.tools/` files are local
build/download assets, not a distributable installer. Installed Rust execution
does not need a project Python environment.

The default regression command is
`cargo test --workspace --no-fail-fast`. Real-model tests require explicit assets;
see [model artifacts](../crates/agentlaw-worker/ARTIFACTS.md).

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
targeting the active user profile. This task has not changed that profile.

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
An `installed` response means replacement, probe and cleanup all finished;
restart the harness normally and use Agentlaw immediately. An incomplete result
names its blocker and never asks for a restart as if installation succeeded.
Retry the same `agentlaw update` command to resume an interrupted plan. The
installed runtime checks unfinished plans on this explicit command before
checking for a newer release; ordinary MCP memory calls do not scan them. A
source build using `AGENTLAW_HOME` for an installed state cannot approve a
managed update.

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
In `doctor` output, `model_load` and `vector_index:<repository>` entries under
`worker_runtime.diagnostics` are unresolved failures. The separate
`worker_runtime.diagnostic_history` keeps the latest verified resolution of each
such failure with its resolution time; it does not reconstruct when the
original failure occurred. The worker snapshot is not a live model probe, and
the top-level `doctor` status still reports source and local integrity.
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
unsupported, without silently reconfiguring Git. Default capture/read-set bounds
and unmeasured performance are documented in [verification](verification.md).

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

## Verification boundary

The implementation record distinguishes code, historical evidence and untested
changes. The regression suite and separate real-model tests cover local Windows
CPU inference, installation, worker recovery and nonempty Git-clone reconstruction.
Actual GPU execution, macOS/Linux/Arm, physical power loss and
million-record user-corpus latency/quality remain unverified.

Building the product does not reconfigure an active harness.
Installation and Git sharing remain explicit operations.
