# Agentlaw 0.2.5

- Fixes `agentlaw doctor` reporting old `model_load` and `vector_index:<repository>`
  failures as active after the worker or vector index has recovered. Recovery is
  verified before the active diagnostic is cleared.
- Preserves the most recently resolved cause for each key in
  `worker_runtime.diagnostic_history`, with the time recovery was confirmed.
  `doctor` remains read-only, and its top-level status retains its existing meaning.
- Lets vector indexing continue for other repositories when one repository fails.
  This release does not change the tool schema or memory storage format.

The GitHub release contains binaries and installers for the supported platforms.
Publishing this release does not replace a running Codex MCP process or migrate an
existing installation. Follow `docs/usage.md` when upgrading a live installation.
