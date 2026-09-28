# Agentlaw 0.3.0

- Checks the latest published full GitHub release in the background without
  delaying memory work. A newer release appears as a conditional notice in
  ordinary MCP tool results, and incomplete updates and old sessions receive
  distinct notices.
- Adds an explicit managed update flow: pinned release preview, checksum and
  archive verification, independently staged helper, offline process check,
  whole-bundle replacement, owned harness registration refresh and same-plan
  recovery. The previous bundle and versioned executables remain available.
- Adds optional `agentlaw support star` with authenticated account verification
  and a separate interactive `y/N` choice. Agentlaw never stars the repository
  as a side effect of an update.

Existing MCP sessions must restart to load a newly installed binary and tool
guidance. Read [the managed update procedure](usage.md#managed-updates-and-optional-github-support)
before applying a release to a registered harness. Process inspection can detect
an active old process but cannot prevent one from starting afterward; keep the
installation offline through application.
