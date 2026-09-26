# Agentlaw 0.2.1

- Ships the optional `skills/agentlaw` usage skill with README installation
  instructions: project-aware connection checks, recall/save boundaries,
  learned procedures and explicit recovery guidance.
- MCP and `agentlaw schema` expose a single object with explicit typed fields,
  enums and action-specific usage descriptions. The separate internal validator
  retains strict action/field checks; no new tools or request fields are added.
  The public tool contract no longer describes itself as a design draft.
- Missing project/store connections explain the next action and concrete input
  shape. Project discovery asks for explicit selection even for one candidate.
- README documents model setup, MCP registration, memory-store selection and
  project discovery/connection as distinct steps, with JSON examples.
- Default installation state and machine-local coordination directories use
  `Agentlaw`, without transitional product naming. Existing data is not moved
  automatically; follow the [path handoff guidance](usage.md#isolated-setup-and-installation)
  before upgrading an installation using the previous directory layout.

Install with the attached shell/PowerShell installer or platform archive. To
update a registered Codex server, first complete any required path handoff and
set `AGENTLAW_HOME` to the existing installation state. Then run the new executable's
`agentlaw install --harness codex --confirm-install`, then restart Codex to reload
the tool schema. Existing model assets, memory and unrelated settings are preserved.

The release workflow tests each target before publishing. Model assets remain a
separate setup step; this update does not migrate legacy Python data.
