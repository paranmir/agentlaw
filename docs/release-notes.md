# Agentlaw 0.2.2

- Keeps one MCP tool, with explicit typed input objects for `recall`,
  `remember_this`, `history`, and `connect_project_memory`. Set `action` and
  supply its same-named object; other action objects must be omitted.
- Separates action fields so history search limits are no longer presented
  beside recall limits in the same input object. Invalid fields and types return
  actionable guidance without echoing unknown field names or supplied values.
- Accepts older flat requests for compatibility through the same strict runtime
  validator. Mixed flat/grouped requests and mismatched action objects fail.
- Updates setup examples, project-connection recovery, bootstrap and usage skill
  to use the grouped form. Memory storage and existing model assets are unchanged.

Update the registered executable with `agentlaw install --harness codex
--confirm-install`, using the existing `AGENTLAW_HOME`, then restart the harness
to reload the tool schema. Update the optional Agentlaw skill as well. The actual
model-facing schema must be checked after restart; CLI success alone does not
establish that the harness displays nested fields correctly.
