# Verification and development status

This repository contains the Rust implementation. Historical Python releases are
a different product. Binary publication is gated by the release workflow; passing
that workflow is not a claim of complete model, harness or search-quality validation.

## Regression suite

For 0.2.1, the local Windows workspace suite passed **145 ordinary tests** with
zero failures (the same three opt-in real-model tests remain ignored). New checks
cover inline schema visibility and equivalence for valid/invalid contract
fixtures, parseable connection recovery instructions, and discovery without
implicit binding. The application tests verify MCP tools/list uses that schema.
The current already-open Codex session may retain its previous tool declaration;
its final model-visible rendering must be checked after restart.

The 0.2.1 optimized local build was installed through `install --harness codex
--confirm-install`. Memory-store selection, machine identity/name, model manifest
and unrelated Codex configuration/bootstrap content were checked unchanged.
Installed CLI recall and project discovery returned the new recovery instructions
without binding a project. Local `doctor` passed source/local integrity checks.
An additional manually launched stdio probe was blocked by the execution policy;
it is not counted as a passing live MCP check.

```sh
cargo fmt --all -- --check
cargo test --locked --workspace --no-fail-fast
```

The public checkout independently passed formatting, a locked offline workspace
build, **143 ordinary tests (0 failures; 3 real-model tests ignored)**, CLI schema/help
and an isolated MCP initialize/tools-list exchange on Windows x64 with Rust/Cargo
1.98.1 and `x86_64-pc-windows-gnullvm`. It compiled without the authoring checkout's
documents or build output. Earlier authoring runs also passed three provisioned
real-model tests; these were not repeated for source relocation.

GitHub Actions uses native Windows/MSVC, Linux and macOS runners for the release
gate. Windows release builds statically link the MSVC C runtime instead of shipping
the local LLVM-MinGW development build and its PATH-dependent runtime DLLs.

Tests cover invalid input, stale revisions, concurrent writers, dependency-review
invalidation, interrupted multi-memory writes and recovery, index corruption,
failed store switching, large complete responses, explicit Git review and import,
worker retry/ownership, and LF/CRLF tool descriptions. This is not a happy-path-only
suite or a claim that every failure is covered.

## Opt-in real-model tests

Provision the files documented in [ARTIFACTS.md](../crates/agentlaw-worker/ARTIFACTS.md)
and set their absolute paths in `AGENTLAW_SMOKE_MODEL`, `AGENTLAW_SMOKE_TOKENIZER`
and `AGENTLAW_SMOKE_ORT`. On Windows the native recovery test also needs
`AGENTLAW_SMOKE_WORKER` pointing to the freshly built `agentlaw-worker.exe`.

```sh
cargo build --locked --workspace
cargo test -p agentlaw-worker --lib native_worker_death_recovers -- --ignored --nocapture
cargo test -p agentlaw-worker --test process real_onnx_model_over_daemon_ipc -- --ignored --nocapture
cargo test -p agentlaw-app --test frontend installed_real_model -- --ignored --nocapture
```

The tests exercise actual CPU inference, recovery after the owned model process
dies, installation, CLI/MCP calls and rebuilding vectors from a nonempty cloned
memory store. They do not download assets automatically. Passing them does not
measure semantic retrieval quality or establish a latency SLA.

## Still unverified

- Live Codex Desktop conversations and actual Oh My Pi model turns. Isolated
  adapter/configuration tests are not the same thing.
- Real-model/live-harness behavior on Linux/macOS/Arm and actual GPU execution.
- Million-record user-corpus retrieval quality, total RSS and latency.
- Physical power-loss/torn-sector behavior beyond the implemented fault-injection
  and process-abort cases.
- Every idle/reattach/orphan and control-channel-loss interleaving.
- Whether a particular LLM will always recall and save at the intended moments.

Ordinary installation preserves the `AgentlawNext` state-directory name to avoid
silently moving existing Rust state or consuming legacy Python memory. Source
publication does not install into an active user profile or migrate old data.
