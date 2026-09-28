# Agentlaw 0.3.4

- Records managed update completion only after candidate verification, approved
  cleanup and maintenance gate closure. Interrupted finalization resumes from
  the matching plan without disrupting an already running candidate MCP.
- Checks only Agentlaw-owned harness registration and bootstrap content when
  judging an installed update; unrelated host settings and user instructions
  no longer make a completed update appear incomplete.
- Removes historical plan discovery from ordinary MCP results. Automatic
  `update_notice` remains available for verified newer releases; the public
  update command resumes one unfinished plan before checking for another
  release, then reports its own installation result or exact blocker.

No new settings, dependencies or persisted file types are introduced. Restart
the harness normally after the public update command reports `installed`.
