# Agentlaw 0.3.1

- Clarifies when to checkpoint an active Task before extended work if unsaved
  findings or failed attempts would be costly to reconstruct.
- Directs project work to recall active Tasks after compaction or a session
  interruption, even when a summary is available, then reconcile both with
  current evidence.
- Makes Task handoffs distinguish verified findings from hypotheses and retain
  the next exact action. This release changes agent guidance, not the memory
  data format or public tool input schema.

Existing MCP sessions must restart to load a newly installed binary and tool
guidance. Read [the managed update procedure](usage.md#managed-updates-and-optional-github-support)
before applying a release to a registered harness. Process inspection can detect
an active old process but cannot prevent one from starting afterward; keep the
installation offline through application.
