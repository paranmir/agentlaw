# Agentlaw 0.3.2

- Adds a stable public command that completes a managed update in one call:
  verify the release, drain Agentlaw processes, replace the bundle and owned
  registrations, probe the new MCP, and remove approved update debris.
- Reports `installed` only after the matching candidate probe, recovery closure
  and cleanup. The harness restart then loads a ready installation.
- Preserves memory, model assets, machine identity and pending work and stops
  before publication if registration ownership or effective paths drift.

Read the
[managed update guide](usage.md#managed-updates-and-optional-github-support)
for status and recovery behavior.
