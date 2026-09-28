# Agentlaw 0.2.3

- Places the executable, private state, memory and model assets under one
  installation root chosen from the current user profile or an explicit path.
  No user name is hard-coded into the default.
- Adds guarded Windows installation and upgrade steps, including archive probes,
  interrupted-swap recovery and rollback. Existing AppData state and memory are
  not migrated automatically.
- Resolves the managed installation root from the executable and its marker,
  keeps source coordination in the selected state directory, and documents
  explicit migration and harness-registration checks.
- Streamlines Agentlaw's host bootstrap and skill guidance for proactive recall,
  task tracking and memory updates.

The GitHub release contains binaries and installers for the supported platforms.
Publishing this release does not replace a running Codex MCP process or migrate an
existing installation. Follow `docs/usage.md` when upgrading a live installation.
