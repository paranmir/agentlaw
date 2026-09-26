---
name: agentlaw
description: Use Agentlaw for persistent memory and learned procedures across tasks and sessions. Apply when checking or setting up Agentlaw connections, resuming work from memory, preserving decisions or corrections, or handling missing context in an Agentlaw-enabled workspace.
---

# Agentlaw

Agentlaw preserves context; the LLM judges meaning and applicability. Follow the
single `agentlaw` tool's current schema. Learned procedures are saved instructions,
not installed harness skills. Answer in the user's language without routine memory reports.

Use exactly the action's same-named input object, for example
`{"action":"recall","recall":{"recall_for":"Current work"}}`. All fields below
belong inside that object. Recall limits are `memory_candidate_limit` and
`procedure_candidate_limit`; `max_matches` belongs only to `history` search.
Follow the exposed schema on older installations rather than sending unsupported nesting.

## Establish the right context

Verify the working project folder through harness/work tools, not the MCP location
or memory store. A home-directory cwd alone does not establish a project. Ask when
unclear; without a project, recall user/machine context only.

For project work, `recall` with verified `project_path`, request-specific `recall_for`
and `include_active_tasks=true`. Set `restore_context=true` for missing/incomplete
context, including after compaction. Reuse applicable context rather than restoring every turn.

## When asked whether Agentlaw is connected

Perform that project recall, not just tool discovery or user-only recall. Inspect
errors and diagnostics. Empty success means no returned matches, not disconnection
or an empty store. Never save a dummy memory as a connectivity test.

- Tool missing: discover if supported; otherwise use the installed CLI, configured
  state directory and `agentlaw schema`. CLI success does not prove MCP connectivity.
- Store unavailable: follow returned recovery guidance. Connect a local Markdown
  store or confirm creation/location. A GitHub URL is not its local path. Do not reset state.
- `project_connection_required` with `status="needs_user_input"`: recall has
  already discovered candidates. Use an explicit user choice if given; otherwise
  ask which candidate to connect, whether to create a new project, or whether to
  skip. A sole candidate is not permission to auto-connect. Use
  `connect_project_memory` with `intent="connect"` and the selected `project_id`,
  or `intent="create"` and `project_name` only after confirmed first adoption.
  Use any returned `recall_result`; otherwise retry recall. Call
  `intent="discover"` only if candidates are missing or need fresh discovery.
- Existing folder binding: reuse it across sessions; do not reconnect routinely.

Report verified access, project folder, recall outcome and degraded/unverified
capabilities. Without a project, explicitly say project connection was not checked.

## During work

| Situation | Action |
| --- | --- |
| New code/document area, changed assumption, dependency, contradiction or failure | `recall` with the new observation and what needs checking, even without known links. |
| Need a known memory/procedure | `recall` by returned IDs; obtain required missing references using returned guidance. |
| Decision, correction, new evidence, command/tool friction or progress worth carrying forward | `remember_this`; a single small mistake counts when its correction is reusable. Preserve its scope and corrected behavior; batch related updates. |
| Need past decisions or changes | `history` for the known memory/procedure, using its schema and returned range/search guidance. |
| Repeated friction suggests a reusable procedure | Review the evidence and use `remember_this`'s procedure-authoring flow; follow returned instructions rather than inventing a tool. |

Choose `create`, `evolve` or `consolidate` explicitly. Copy returned IDs/versions.
Evolve complete current content, preserving exceptions; do not append a log.
Separate understanding from evidence and select user/project/machine scope deliberately.
Maintain unfinished Tasks and record closure using the schema's headings, without
copying the Plan. Save clear corrections before dependent work; at progress
boundaries and before the final response, save only useful changes not already
reflected in current memory. Skip incidental chatter, not new relevant evidence.
Label assistant proposals and unverified inferences; do not present them as user
decisions or verified facts. A write attempt is not a successful save.

## Resolve, do not conceal

A retained proposal is not published. Continue review through `remember_this` using
returned references; do not resubmit unchanged proposals. Ask the user to resolve
conflicting standing rules or decisions requiring their choice.

If required context is unavailable, report it and pause dependent changes. Lexical-only
success is not semantic readiness. Use CLI `doctor` when needed; repair only within
authorization. Git sharing is separate from saving and requires sensitive-pattern
inspection and the appropriate user decision.
