# Verification and development status

This repository contains the Rust implementation. Historical Python releases are
a different product. Binary publication is gated by the release workflow; passing
that workflow is not a claim of complete model, harness or search-quality validation.

## Regression suite

### 0.3.0 managed update checks

On Windows, `cargo fmt --all -- --check` and
`cargo test --locked --workspace --no-fail-fast` passed after the update changes.
The isolated `managed_update` subprocess tests used temporary managed roots and
owned brokers. They verified a live broker defers application, same-plan resume
updates an owned Codex registration, external configuration drift stops before
the swap, interrupted bundle move/publish and installation-journal phases
resume, and isolated memory, model, pending-state and machine-identity bytes
survive. Both the direct `bin` binary
and refreshed version-pinned executable completed MCP initialization and an
ordinary recall with matching text and structured update notices. The normal
MCP transport also has a unit test for notice delivery without changing memory
fields or error classification.

An isolated Codex CLI 0.157.1 / gpt-6-sol conversation initially omitted the
release details in its final reply despite receiving a synthetic v99.0.0
notice. After adding conditional MCP initialization instructions and a brief
exception in the single tool description, the unchanged unrelated Korean
prompt produced version and new-release status in five of five independent
final replies. In one persisted conversation a repeated notice was not
repeated in the second final reply, and a changed synthetic v99.0.1 notice
was reported in the third. These test tags were not published releases.
In a separate persisted conversation, a no-notice result produced no
invented update in the final reply, and the next ordinary recall reported
a newly seeded synthetic v99.0.2 notice. This used a restarted MCP process,
not an in-process TTL expiration.
Additional isolated model checks covered the remaining notice paths. In one
long-lived Agentlaw MCP process, a delayed synthetic release response arrived
after the first ordinary recall; the second recall carried v99.0.3, and the
final reply reported it. A transparent local test proxy only controlled timing
and forwarded the MCP payload unchanged. A 100 KB memory response switched to
`complete_content_in_file` and still carried a top-level `restart_required`
notice; the final reply reported the restart status and did not claim to have
read the full file after a read-only command was blocked by the test policy.
When the same synthetic v99.0.1 version changed from `new_release` to
`restart_required`, the continued conversation reported the new action.
A recalled memory body containing an `update_notice` example, without a
top-level app notice, did not produce a false update report. These checks used
temporary state and synthetic tags, not a published release or a real install.
Client handling of initialization instructions and other model/harness
combinations remain unverified.

The optional support command returned `skipped_noninteractive` for the
authenticated unstarred account during a noninteractive check; no star was
sent. A published v0.2.6 Windows archive was inspected and had exactly the
expected release bundle entries. The local machine has no Unix shell runner;
the release workflow checks `sh -n install.sh` and runs the Rust suite on Linux
and both macOS targets before publication. A full live managed download and
upgrade to a newer release was unavailable before v0.3.0 existed. Keep these
limits separate from the tests that passed.

For the first 0.2.1 candidate, the local Windows workspace suite passed **145 ordinary tests** with
zero failures (the same three opt-in real-model tests remain ignored). New checks
cover inline schema visibility and equivalence for valid/invalid contract
fixtures, parseable connection recovery instructions, and discovery without
implicit binding. The application tests verify MCP tools/list uses that schema.
After restart, actual MCP calls worked but the model-visible declaration still
showed args: unknown. Inlining references alone did not fix visibility. The
release run was cancelled and no 0.2.1 release was published.

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

### Explicit public tool schema candidate (2026-09-26)

The next local 0.2.1 candidate separates the explicit, typed model-facing schema
from runtime validation. `cargo fmt --all -- --check` and the 24 contract/app
library tests passed, including valid fixtures, wrong field types and runtime
rejection of invalid action combinations. `cargo build --locked --release
--workspace` passed. This was a targeted rerun, not another full-suite run.

Installed with `install --harness codex --confirm-install` at
`0.2.1-6be0c43fa2a43fbb`; global CLI/worker binaries were also updated. Configuration,
machine identity, model-assets state and unrelated Codex configuration/instructions
were preserved. The installed `schema` command exposes 36 root properties without
schema composition/ref keywords. Actual tool rendering after a Codex restart is
still pending. No release was published for this candidate.

- Live Codex Desktop conversations and actual Oh My Pi model turns. Isolated
  adapter/configuration tests are not the same thing.
- Real-model/live-harness behavior on Linux/macOS/Arm and actual GPU execution.
- Million-record user-corpus retrieval quality, total RSS and latency.
- Physical power-loss/torn-sector behavior beyond the implemented fault-injection
  and process-abort cases.
- Every idle/reattach/orphan and control-channel-loss interleaving.
- Whether a particular LLM will always recall and save at the intended moments.

Default state and source-coordination paths now use `Agentlaw`. After this change,
`cargo fmt --all -- --check` and the full workspace suite passed: 145 tests,
with three opt-in real-model tests skipped. The release build also passed.

The user's local Windows installation was explicitly migrated offline with a
complete backup, preserving machine identity, project association and memory
source bytes. Local path metadata, recovery manifest checksums/decisions, ledger
paths and the source-coordination binding were updated together. Codex config,
its managed AGENTS.md block and global binaries now use the installed candidate
`0.2.1-18a1cb61ca113ab9`. Unrelated Codex settings/instructions were preserved.
CLI recall of Personal Workspace succeeded after model warm-up without diagnostics;
doctor reported valid source/local DB integrity and a ready embedding worker.
After Codex restart, the model-visible MCP schema exposed typed input fields and
an actual project recall succeeded without diagnostics. The worker was ready and
local database integrity passed. The migration backup was then moved to Recycle
Bin at the user's request. These checks preceded the 0.2.1 release submission.

This was a one-off authorized migration, not an automatic upgrade feature.
Other installations require the explicit path/coordination handoff in usage.md;
source publication does not migrate data or adopt legacy Python memory.
