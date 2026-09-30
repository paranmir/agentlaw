# Verification and development status

This repository contains the Rust implementation. Binary publication is gated
by the release workflow. Passing that workflow establishes only the checks
recorded below.

## Regression suite

### 0.3.5 contextual standing-rule recall checks

Product PR [#9](https://github.com/paranmir/agentlaw/pull/9) passed the full
locked workspace test suite on Linux x64, Windows x64, macOS arm64 and macOS
x64 in [CI run 36544489655](https://github.com/paranmir/agentlaw/actions/runs/36544489655).
Pure-flow and Runtime tests cover general contextual recall with low candidate
limits, explicit-ID/context combinations, ID-only boundaries, required
references, same-head rule/scope matching, compound scope matching, stale-index
synchronization before recall, and unconnected-project partial recall.
Streaming and CLI tests parse complete oversized rule artifacts, including
aggregate rule/reference content and unconnected-project partial results.

These tests do not inject mid-call source changes, partial-recall internal
failure, artifact-creation failure or partial required-reference read failure.
They also do not establish that an LLM always calls recall or follows every
delivered rule. Release packaging is checked by the publication workflow;
installation into a running user harness is a separate check.

### 0.3.4 managed update completion checks

The ordinary MCP notice test covers more than 64 historical update plans and
confirms they are not inspected for an automatic incomplete/restart notice.
The isolated managed-update tests check finalization after cleanup and gate
closure, unrelated host edits, owned registration drift, retry through the
same plan ID, and deferred cleanup. An opt-in Windows test copied the released
v0.3.3 stable launcher into an isolated installation, interrupted a completed
fixture at `finalizing` with its gate closed and stage removed, then ran the
public `update` command. It returned `installed` for that same ID without
changing the launcher or creating a new plan. This verifies the old-launcher
handoff for that specific interruption window, not all crash points or other
platforms. On Windows, `cargo fmt --all -- --check` and
`cargo test --locked --workspace --no-fail-fast` passed after the runtime
changes; all five ordinary managed-update integration tests passed. Three
real-model asset tests remained ignored. The release workflow and installed
v0.3.4 remain to be verified.

### 0.3.3 Task handoff guidance checks

The v0.3.3 candidate changes LLM-facing guidance, not memory storage or the
tool input schema. The accepted workspace/product tool and bootstrap text blocks
match, and the generated bootstrap source contains the accepted paragraph.
`cargo fmt --all -- --check` and `cargo build --locked -p agentlaw-app` passed.
The candidate CLI reported v0.3.3 with the accepted `schema` description; an
isolated MCP `initialize` reported v0.3.3 and `tools/list` exposed that same
description. These checks establish candidate exposure, not behavior of a
running user harness after update. Long review-edit-test interruption scenarios
for Sol and Astra remain unverified. They must judge whether the last confirmed
Task and accessible references let a fresh session resume; call count alone is
not a success criterion. Do not infer universal compliance from a short run.

### 0.3.2 synchronous replacement checks

The stop, replace, probe and cleanup design supersedes the earlier live
coexistence tests. The isolated Windows integration suite now checks exact
cleanup before success, an open MCP session draining before replacement, and
harness drift stopping before publication. Full locked workspace and release
matrix results must be recorded after the implementation is finalized; these
focused tests alone do not establish cross-platform behavior.

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

## v0.3.2 managed updater candidate (2026-09-28)

The isolated Windows managed-update integration suite passed 3 tests for
replacement, candidate MCP probe and cleanup, open-session drain, and a
registration drift blocker. `cargo fmt --all -- --check` and
`cargo test --locked --workspace --no-fail-fast` passed after the synchronous
updater changes. Tests did not modify the active Agentlaw installation.
The four-platform release matrix, public release assets, and local installation
are pending separate verification.

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
