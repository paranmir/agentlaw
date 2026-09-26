# Agentlaw 0.2.1

- MCP and `agentlaw schema` expose inline action and nested field definitions,
  rather than requiring clients to resolve `$ref`/`$defs`. Input validation is
  unchanged; the executable contract no longer describes itself as a design draft.
- Missing project/store connections explain the next action and concrete input
  shape. Project discovery asks for explicit selection even for one candidate.
- README documents model setup, MCP registration, memory-store selection and
  project discovery/connection as distinct steps, with JSON examples.

Install with the attached shell/PowerShell installer or platform archive. To
update a registered Codex server, run the new executable's
`agentlaw install --harness codex --confirm-install`, then restart Codex to reload
the tool schema. Existing model assets, memory and unrelated settings are preserved.

The release workflow tests each target before publishing. Model assets remain a
separate setup step; this update does not migrate legacy Python data.
