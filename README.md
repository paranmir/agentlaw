<div align="center">

# Agentlaw

**Persistent memory for the AI agents you already use.**

Keep decisions, corrections and working context across sessions, without replacing your agent harness.

[![Release workflow](https://github.com/paranmir/agentlaw/actions/workflows/release.yml/badge.svg)](https://github.com/paranmir/agentlaw/actions/workflows/release.yml)
[![License](https://img.shields.io/github/license/paranmir/agentlaw)](LICENSE)

[Getting started](#getting-started) · [Usage guide](docs/usage.md) · [Model setup](crates/agentlaw-worker/ARTIFACTS.md) · [Verification](docs/verification.md)

</div>

---

Agentlaw is a Rust memory runtime with one MCP tool, `agentlaw`, and a CLI using
the same request contract. The agent decides what a memory means; Agentlaw handles
retrieval, explicit updates, history, indexing and recovery.

- **Continue the work.** Recall project context, standing rules, active tasks and
  learned procedures when a session starts or the work changes.
- **Keep the original.** Current memories are complete Markdown documents.
  History is preserved separately; search indexes and embeddings are rebuildable.
- **Carry context between machines.** Share a memory repository through explicit
  Git operations and rebuild local indexes from its source documents.
- **Update a managed installation.** After you approve a pinned release, the
  assistant can apply a certified compatible update and verify it before asking
  you to restart the harness normally.

> **Rust development release.** Version 0.3.2 is the current Rust release;
> version 0.2.0 started this implementation. Older tags and the Python package describe the previous
> product. Do not use `pip install agentlaw` or the old governance initialization
> instructions to install this code. See [what has been verified](docs/verification.md).

## Getting started

### Linux and macOS — curl or wget

```sh
curl -fsSL https://github.com/paranmir/agentlaw/releases/latest/download/install.sh | sh
# Or:
wget -qO- https://github.com/paranmir/agentlaw/releases/latest/download/install.sh | sh
```

The installer checks the archive's SHA-256 and installs to
`~/Agentlaw/bin`, without sudo or shell-profile edits. Add it to PATH:

```sh
export PATH="$HOME/Agentlaw/bin:$PATH"
agentlaw --version
```

Put the export line in your shell profile to keep it for new terminals.
Set `AGENTLAW_ROOT` to choose another complete installation root, or
`AGENTLAW_VERSION=v0.3.2` to pin this release. The installer keeps binaries in
`bin`, private state in `state`, memory in a sibling `memory` directory once
connected, and model assets in `models`. It does not migrate an older state
automatically. Download the script first if you want to inspect it before execution.

### Windows — PowerShell

```powershell
irm https://github.com/paranmir/agentlaw/releases/latest/download/install.ps1 | iex
agentlaw --version
```

For a fresh installation, the default root is the current Windows user's profile
folder plus `Agentlaw` (for example, `%USERPROFILE%\Agentlaw`, not a hard-coded
user name). The installer puts executables in `bin`, creates private `state`,
and records a layout marker. It proposes sibling `memory` without creating it;
model assets installed during harness setup go under `models`. It rejects roots
inside AppData to avoid packaged-app AppData redirection. The installer adds
`bin` to the user PATH. No administrator privileges or execution-policy changes
are required. New terminals pick up the persisted PATH. To inspect the script
and choose a different root:

```powershell
Invoke-WebRequest https://github.com/paranmir/agentlaw/releases/latest/download/install.ps1 -OutFile install.ps1
Get-Content ./install.ps1
# After inspecting the file:
& ([scriptblock]::Create((Get-Content ./install.ps1 -Raw))) -RootDir (Join-Path ([Environment]::GetFolderPath('UserProfile')) 'Tools/Agentlaw')
```

`-RootDir` selects the whole installation, not just the executable directory.
The installer stops when the selected root contains unrecognized state or
another managed root is already selected. One OS user shares one installation
across harnesses; update that installation instead.
For a nondefault root, pass the same `-RootDir` on upgrades.
Re-running the installer at the same managed root updates executables; it does
not reset memory or configure a harness.

Agentlaw can report a newer published release in an ordinary MCP result. For a
managed installation, run `agentlaw update` through the stable command in
`command/`. It verifies the release, stops the affected Agentlaw processes,
replaces the bundle and owned registrations, probes the candidate MCP, and
removes approved old update artifacts before reporting `installed`. Then
restart the harness normally. See [the update procedure](docs/usage.md#managed-updates-and-optional-github-support).

The root contains `bin/`, `models/`, `memory/`, and `state/`. Configuration,
machine identity, executable versions, worker files, recovery data, indexes,
and source-coordination locks all live under `state/`. A custom root does not
create another registry under the default root. Build tools are development
dependencies, not release-installation contents.

### Direct download

Download an archive and `SHA256SUMS` from [GitHub Releases](https://github.com/paranmir/agentlaw/releases).
Extract the binaries into a directory on PATH and verify the archive with
`sha256sum`, `shasum -a 256`, or `Get-FileHash -Algorithm SHA256`.
For a standalone extracted binary, set `AGENTLAW_HOME` to an explicit absolute
state directory; only managed installations discover their state automatically.

| Platform | Release archive |
| --- | --- |
| Windows x64 | `agentlaw-x86_64-pc-windows-msvc.zip` |
| Linux x64 (glibc 2.35+) | `agentlaw-x86_64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `agentlaw-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `agentlaw-x86_64-apple-darwin.tar.gz` |

### Build from source / Cargo

Prerequisites: Git, Rust/Cargo and the native linker required by your Rust target.
The checked-in lockfile was tested with Rust 1.98.1 on Windows x64. Rust execution
does not require Python; Python is used only for the optional model conversion tool.

```sh
git clone https://github.com/paranmir/agentlaw.git
cd agentlaw
cargo build --locked --workspace
```

Run `./target/debug/agentlaw --help` on Unix-like systems, or
`.\target\debug\agentlaw.exe --help` in PowerShell. `schema` prints the actual MCP
tool definition and input schema. Platform-specific verification is listed in
[Verification](docs/verification.md); these commands are not a claim of tested
support on every platform.

To build optimized executables, add `--release`; outputs are under `target/release/`.
Alternatively, install the CLI with Cargo from the cloned repository:

```sh
cargo install --locked --path crates/agentlaw-app
```

**The installers provide executables, not model assets or automatic harness setup.**
Model/tokenizer files and native ONNX Runtime libraries are configured separately
below. No private memory, Python migration or active-profile edits are performed
by downloading the binaries.

### Set up memory and a harness

Use the executable path above in place of `agentlaw` until it is on your PATH.
Start with `agentlaw store propose-location`, confirm the proposed location, then
use `store create` or `store connect` as described in the [usage guide](docs/usage.md).

`agentlaw install --harness codex` previews installation targets. It changes a
harness only with `--confirm-install`. Use `--harness-dir` for an isolated profile
when trying it out. Semantic retrieval additionally needs the verified model,
tokenizer and platform ONNX Runtime artifacts described in
[model setup](crates/agentlaw-worker/ARTIFACTS.md).

Codex and Oh My Pi configuration adapters are implemented. Actual Oh My Pi
end-to-end validation is deferred; configuration tests are not live-model validation.

#### Codex setup: binaries, memory store, then project connection

These are separate steps. Registering MCP does not select a memory store or
associate your working folder with a project. It installs MCP configuration and
a short managed `AGENTS.md` bootstrap. The companion usage skill below is installed
separately; neither bootstrap nor skill installation establishes a project binding.

#### Install the Agentlaw usage skill

The [Agentlaw skill](skills/agentlaw/SKILL.md) teaches when to recall, save, inspect
history, use learned procedures and resolve connection problems. In particular,
"Is Agentlaw connected?" must check the actual project's recall, not merely whether
the tool exists. It complements the short bootstrap and live tool schema.

In Codex, ask its skill installer:

```text
Install the agentlaw skill from https://github.com/paranmir/agentlaw,
using the folder skills/agentlaw.
```

Alternatively, copy `skills/agentlaw` from a trusted checkout into
`$CODEX_HOME/skills/agentlaw` (normally `~/.codex/skills/agentlaw`). If that folder
already exists, review it before replacing anything. Restart Codex to load the
skill, then ask "Is Agentlaw connected to this project?" Other skill-capable
harnesses can install the same folder using their documented skill location;
this does not imply tested compatibility with every harness.

The skill is plain Markdown: no extra runtime or model is installed, no memory is
created, and MCP registration is unchanged. Install only the skill folder, not
the repository's contributor `AGENTS.md` as global instructions.

#### Complete Codex setup

1. Prepare the model assets using [model setup](crates/agentlaw-worker/ARTIFACTS.md).
   Preview registration with `agentlaw install --harness codex`, then confirm:

   ```text
   agentlaw install --harness codex --model-manifest <absolute-manifest.json> --confirm-install
   ```

   Omit `--model-manifest` only if you intentionally skip semantic retrieval or
   already have installed assets. Binaries alone do not download the model.

2. Connect the **local Markdown memory store**, not its GitHub URL:

   ```text
   agentlaw store propose-location
   agentlaw store create --path <user-confirmed-absolute-path> --confirm-create
   agentlaw machine name --value <user-chosen-machine-name>
   ```

   For an existing store, use `agentlaw store connect --path <local-store-path>`
   instead of `store create`. Download a shared Git repository first, outside
   this command. Keep the same `AGENTLAW_HOME` for CLI setup and the MCP process.

3. Restart Codex so it loads the installed executable and refreshed tool schema.
   `agentlaw schema` shows the public tool's typed input fields and usage guidance;
   `agentlaw doctor` checks local setup without resetting memory.

4. Have the agent verify the **project's actual root folder** with its workspace
   tools, then call the single MCP tool `agentlaw` to discover project identities:

   ```json
   {
     "action": "connect_project_memory",
     "connect_project_memory": {
       "project_path": "C:/work/my-project",
       "intent": "discover"
     }
   }
   ```

   Replace that example path with the verified folder (for example,
   `/home/me/work/my-project` on Linux). Optional `clues` may contain observed
   `repository_url`, `name`, and `description`; omit unknown values. Never use
   Agentlaw's own directory or memory-store remote as project clues.

5. Discovery does **not** connect automatically, even for one candidate. After
   the user selects the existing project, copy its returned ID:

   ```json
   {
     "action": "connect_project_memory",
     "connect_project_memory": {
       "project_path": "C:/work/my-project",
       "intent": "connect",
       "project_id": "<selected candidate project_id>",
       "restore_context": true,
       "recall_for": "Current project context, decisions and unfinished work"
     }
   }
   ```

   For confirmed first-time adoption, replace `intent` with `"create"`, omit
   `project_id`, and supply `project_name`. An empty candidate list alone does
   not establish that this is a new project.

6. Subsequent project recall uses the verified folder without reconnecting:

   ```json
   {
     "action": "recall",
     "recall": {
       "project_path": "C:/work/my-project",
       "recall_for": "Context needed for the current request",
       "include_active_tasks": true,
       "restore_context": true
     }
   }
   ```

   Use `restore_context` when project context is missing or incomplete; omit it
   for focused follow-up. Without project work, omit `project_path` and use
   user/machine recall. CLI fallback accepts these same JSON requests on stdin
   through `agentlaw call --json -`.

If the tool returns `project_connection_required`, follow its `next_action` to
discover and explicitly connect a project, then retry the original recall.
`memory_store_connection_required` means step 2 is needed first. Neither means
"there are no memories". Instructions to the agent are in English; explanations
and confirmation questions should use the user's language.

## One tool, deliberate operations

Set `action` and put its inputs inside the same-named object; omit the other
action objects. Every nested field has an explicit type in the MCP schema.
For recall, use `memory_candidate_limit` or `procedure_candidate_limit`;
`max_matches` is only available inside `history`. Flat and mixed requests are
rejected.

| Action | Purpose |
| --- | --- |
| `recall` | Retrieve relevant context or known memory IDs; restore applicable project context when requested |
| `remember_this` | Create, evolve or consolidate memories, with explicit evidence and conflict review |
| `history` | Inspect preserved changes and their evidence |
| `connect_project_memory` | Associate a verified project folder with a memory-store project identity |

`agentlaw mcp serve --stdio` exposes the MCP server.
`agentlaw call --json -` reads a request from stdin and writes its result to stdout.
The [usage guide](docs/usage.md) covers setup, installation, history and Git sharing.
The [public tool schema](docs/design/contracts/agentlaw-tool.schema.json) explicitly
describes each field, type and action. The separate
[runtime validation schema](docs/design/contracts/agentlaw-input.schema.json)
enforces action-specific requirements and rejects incompatible combinations
before execution. [Examples](docs/design/contracts/agentlaw-input.examples.json)
cover both valid and invalid requests. There is still only one MCP tool, and
the CLI uses the same JSON requests; unused fields should be omitted.

## Architecture

| Crate | Responsibility |
| --- | --- |
| `agentlaw-app` | MCP/CLI, configuration, installation and Git operations |
| `agentlaw-contracts` | Shared types, validated inputs and LLM-facing guidance |
| `agentlaw-flows` | Context, recall, memory review, procedures and history |
| `agentlaw-storage` | Markdown current state, lossless history and recoverable publication |
| `agentlaw-search` | BM25/vector retrieval, rank fusion, scopes and index generations |
| `agentlaw-worker` | Shared broker, model lifecycle and durable indexing jobs |

There is no hidden background LLM making semantic decisions. The working agent
interprets conflicting or related memories, and asks the user when a decision
requires their input. Tool registration alone does not guarantee that every model
will invoke the tool correctly on every turn.

## Development

```sh
cargo fmt --all -- --check
cargo test --locked --workspace --no-fail-fast
```

Real-model tests are explicitly opt-in and require local model/runtime assets.
See [verification](docs/verification.md) for their commands and limitations.
Build artifacts, installed state and model downloads must not be committed.

### Releasing

The [Release workflow](.github/workflows/release.yml) runs the ordinary tests on
each release target before building and publishing. Trigger it with **Actions →
Release → Run workflow**, or push a `v` tag matching the workspace version in
`Cargo.toml`. A manual run creates that tag on the tested commit. Existing releases
are not overwritten. PRs run tests without publishing. Real-model tests are not
downloaded or run as part of this ordinary release gate.

## Security and license

Git sharing checks outgoing history for identifiable secret patterns and requires
an explicit decision for flagged content. Private hosting is not a substitute for
that decision. See [SECURITY.md](SECURITY.md) for private vulnerability reporting.

Agentlaw source is [MIT licensed](LICENSE). Model and ONNX Runtime artifacts keep
their respective upstream licenses; this source license does not replace them.
