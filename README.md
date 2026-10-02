<div align="center">

# Agentlaw

**Persistent memory for the AI agent you already work with.**

Pick up unfinished work, carry decisions into a new session, and correct what your agent remembers.

[See the experience](#what-using-agentlaw-looks-like) · [Get started](#get-started) · [Installation guide](docs/installation.md) · [Usage guide](docs/usage.md)

[![Release workflow](https://github.com/paranmir/agentlaw/actions/workflows/release.yml/badge.svg)](https://github.com/paranmir/agentlaw/actions/workflows/release.yml)
[![License](https://img.shields.io/github/license/paranmir/agentlaw)](LICENSE)

</div>

## What using Agentlaw looks like

You work in your usual agent harness. Agentlaw gives that agent a place to save what should survive the conversation.

> **During a project:** “We decided to keep the update command simple. Save that decision and where the work stands.”
>
> **In a later session:** “Continue the update work.”
>
> **Your agent, after recall:** “The agreed direction was one update command. The next step is to verify the interrupted-install path.”
>
> **When you correct it:** “The cleanup must finish before we call the update complete.”
>
> **Your agent:** Updates the relevant memory, keeping the correction and its reason in history.

The same flow works for preferences, project decisions, reusable procedures, and long tasks. A Task keeps the current objective, position, and next step so another session can resume from a useful handoff.

## Why keep memory here?

- **Resume real work.** Carry a compact Task handoff across sessions and context resets.
- **Keep corrections.** Update a remembered decision when new evidence or your direction changes it.
- **Find the right context.** Recall relevant memories and active Tasks for the project at hand.
- **See how an answer changed.** Inspect the history and evidence behind a memory.
- **Own the source.** Current memories are Markdown documents. You choose the local store and can share its source through explicit Git operations.

Agentlaw connects to an existing harness through one MCP tool. Its CLI uses the same memory operations. Codex has a configuration adapter; an Oh My Pi adapter is also implemented. See [verification](docs/verification.md) for the environments tested so far.

## Get started

### 1. Install Agentlaw

**Windows (PowerShell)**

~~~powershell
irm https://github.com/paranmir/agentlaw/releases/latest/download/install.ps1 | iex
agentlaw --version
~~~

**macOS and Linux**

~~~sh
curl -fsSL https://github.com/paranmir/agentlaw/releases/latest/download/install.sh | sh
export PATH="$HOME/Agentlaw/bin:$PATH"
agentlaw --version
~~~

The [installation guide](docs/installation.md) covers alternative download methods, custom locations, checksums, source builds, and platform details.

### 2. Connect a memory store and your agent

Create or connect a local Markdown memory store, then register Agentlaw with your harness. For semantic recall, add the [model assets](crates/agentlaw-worker/ARTIFACTS.md). The [installation guide](docs/installation.md#set-up-memory-and-a-harness) walks through Codex setup, the Agentlaw skill, and project connection.

Once the harness restarts, open the project you want to work on and ask:

> “Is Agentlaw connected to this project? Recall its decisions and unfinished work.”

The agent can discover an existing project identity or help you create one. After that, work in ordinary language: ask it to remember an important correction, resume a Task, or show why a memory changed.

## How the pieces fit

| What you see | What Agentlaw keeps |
| --- | --- |
| “Continue where we left off.” | A project Task with its objective, current position, and resume point |
| “Remember this decision for future work.” | A scoped Markdown memory with evidence |
| “That decision changed.” | The revised current memory and its earlier versions |
| “Why did we do it this way?” | Relevant memory history and supporting context |

Agentlaw stores the current memory separately from its change history. Search indexes and embeddings can be rebuilt from the source. Project, user, and machine scope keep recalled context attached to where it applies.

## Explore further

- [Installation and Codex setup](docs/installation.md)
- [Everyday memory use, history, sharing, and updates](docs/usage.md)
- [Agentlaw usage skill](skills/agentlaw/SKILL.md)
- [Public tool schema and examples](docs/design/contracts/agentlaw-tool.schema.json)
- [What has been verified](docs/verification.md)

## Development

This is a Rust workspace. To check a source change:

~~~sh
cargo fmt --all -- --check
cargo test --locked --workspace --no-fail-fast
~~~

Real-model tests need local model and runtime assets and are opt-in; see [verification](docs/verification.md). The [candidate workflow](.github/workflows/release.yml) tests and packages each PR once. After a version-changing PR merges, the [publisher](.github/workflows/publish.yml) verifies and releases those same files without rerunning tests or rebuilding. See [release operation and recovery](docs/releasing.md).

## Security and license

Git sharing checks outgoing history for identifiable secret patterns and requires an explicit decision for flagged content. See [SECURITY.md](SECURITY.md) for private vulnerability reporting.

Agentlaw source is [MIT licensed](LICENSE). Model and ONNX Runtime artifacts retain their upstream licenses.
