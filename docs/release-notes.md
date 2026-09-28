# Agentlaw 0.2.6

- Guides agents to recall at a new work request or resumption, including short
  work that may reuse a saved approach. New evidence or a similarity to prior
  work prompts a focused follow-up recall when the question is not covered by
  context already retrieved.
- Tells agents to check active project Tasks and match by objective and scope
  before creating a Task. Repeated recall is skipped only while the relevant
  results remain in context for the same question and conditions.
- Aligns the MCP tool description, installed bootstrap, and optional skill.
  The schema, memory storage format, and Runtime retrieval behavior are unchanged.
  Invocation still depends on the agent and harness following the guidance.

The GitHub release contains binaries and installers for the supported platforms.
Publishing this release does not replace a running Codex MCP process or update
an existing harness bootstrap. Follow `docs/usage.md` to upgrade the executable,
refresh harness instructions, and start a new session with the new tool description.
