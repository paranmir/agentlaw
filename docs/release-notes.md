# Agentlaw 0.3.3

- Clarifies when an agent must checkpoint a Task during extended work: compare
  the last confirmed save with what a fresh session needs, and save before the
  next dependent step or wait rather than deferring to a broad phase end.
- Keeps unfinished work and verification limits in the Task, while avoiding
  duplicate writes when the last confirmed handoff already suffices.
- Treats failed, pending, and unconfirmed memory writes as unsaved and uses
  existing recovery paths; no new memory format, setting, or background process.

The guidance is supplied through the tool description, installed bootstrap,
and Agentlaw skill. Updating a running harness requires a fresh session to load
the new instructions.
