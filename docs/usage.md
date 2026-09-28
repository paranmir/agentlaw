# Agentlaw usage

Agentlaw attaches to an existing agent harness. One MCP tool, `agentlaw`, provides
recall, deliberate memory updates, history and learned procedures. The CLI uses
the same input contract. Runtime does not replace the agent's semantic judgment
or the user's decisions.

This workspace implements the redesign authorized on 2026-09-26 independently
of the legacy Python product. The Rust executable is named `agentlaw`; the
Python package is not an installation route for this implementation.
Publishing the source does not imply a packaged binary release or complete verification.
The user has resumed risk-focused testing after the implementation pass. Oh My Pi
end-to-end verification remains deferred. Passing tests verify only their stated scope.
See the [implementation record](verification.md).
The [practical testing report](verification.md) records
the testing principles, regression scenarios, real-model runs and remaining gaps.

## Modules and source of truth

| Crate | Responsibility |
|---|---|
| agentlaw-contracts | Shared types, schema, validation and LLM-facing guidance |
| agentlaw-flows | Project context, recall, write/review, procedures and history |
| agentlaw-storage | Complete current Markdown, lossless history, publication/recovery |
| agentlaw-search | BM25, vector candidates, RRF, scope filters and generations |
| agentlaw-worker | Shared broker, model lifecycle, durable jobs and index acknowledgment |
| agentlaw-app | MCP/CLI, explicit installation, configuration and Git operations |

The [design guide](../README.md#architecture) identifies normative contracts.
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
The registry is local state, not Git-shared memory. Changing the default does not
move existing data or rewrite registered harness commands. Before upgrading an
installation using a former default, stop its Agentlaw processes and preserve
its state and old source-coordination registry. Migrate source and registry
together, checking existing bindings before replacement; update configured paths
explicitly if moving installation state. Do not run old and new binaries against
the same store with separate coordination registries. The new runtime does not
search old registry locations; offline migration must account for them first.
Index generation acknowledgements include their absolute backend directory.
After an explicit offline relocation has updated source/control/recovery bindings
and harness paths, run `agentlaw repair` from the new installation to rebuild
derived generations. Copying index files alone is not sufficient. Verify a real
semantic recall and subsequent write/index advancement before retiring backups.
`repair` is not a relocation command and does not rewrite those source bindings.
Legacy Python data must not be adopted as Rust state. These path conventions are
not evidence of cross-platform testing.

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
newer full GitHub release, when a managed update is incomplete, or when an old
session needs a restart. The release check is bounded and runs outside memory
work. A failed check does not claim that the installation is current. The
notice does not install anything; a running MCP process keeps its loaded version.

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
Retry the same `agentlaw update` command to resume an interrupted plan. A source
build using `AGENTLAW_HOME` for an installed state cannot approve a managed
update.

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

## Git and management

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

Building the product does not migrate legacy memory or reconfigure an active
harness. Installation and Git sharing remain explicit operations.
