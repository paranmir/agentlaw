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

> **Rust development release.** Version 0.2.0 starts the new Rust implementation.
> Older tags and the Python package describe the previous
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
`~/.local/share/agentlaw/bin`, without sudo or shell-profile edits. Add it to PATH:

```sh
export PATH="$HOME/.local/share/agentlaw/bin:$PATH"
agentlaw --version
```

Put the export line in your shell profile to keep it for new terminals.
Set `AGENTLAW_INSTALL_DIR` to choose another directory, or `AGENTLAW_VERSION=v0.2.0`
to pin a release. Download the script first if you want to inspect it before execution.

### Windows — PowerShell

```powershell
irm https://github.com/paranmir/agentlaw/releases/latest/download/install.ps1 | iex
agentlaw --version
```

Installs to `%LOCALAPPDATA%\Programs\Agentlaw\bin` and adds that directory to your
user PATH. No administrator privileges or execution-policy changes are required.
New terminals pick up the persisted PATH. To inspect the script and choose options:

```powershell
Invoke-WebRequest https://github.com/paranmir/agentlaw/releases/latest/download/install.ps1 -OutFile install.ps1
Get-Content ./install.ps1
# After inspecting the file:
& ([scriptblock]::Create((Get-Content ./install.ps1 -Raw))) -Version v0.2.0
```

`-InstallDir` selects a different installation directory. Re-running either
installer updates the executables; it does not reset memory or configure a harness.

### Direct download

Download an archive and `SHA256SUMS` from [GitHub Releases](https://github.com/paranmir/agentlaw/releases).
Extract the binaries into a directory on PATH and verify the archive with
`sha256sum`, `shasum -a 256`, or `Get-FileHash -Algorithm SHA256`.

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

## One tool, deliberate operations

| Action | Purpose |
| --- | --- |
| `recall` | Retrieve relevant context or known memory IDs; restore applicable project context when requested |
| `remember_this` | Create, evolve or consolidate memories, with explicit evidence and conflict review |
| `history` | Inspect preserved changes and their evidence |
| `connect_project_memory` | Associate a verified project folder with a memory-store project identity |

`agentlaw mcp serve --stdio` exposes the MCP server.
`agentlaw call --json -` reads a request from stdin and writes its result to stdout.
The [usage guide](docs/usage.md) covers setup, installation, history and Git sharing.
The [input schema](docs/design/contracts/agentlaw-input.draft.schema.json) and
[examples](docs/design/contracts/agentlaw-input.examples.json) are included in this repository.

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
