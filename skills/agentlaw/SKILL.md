---
name: agentlaw
description: Use for detailed Agentlaw Task authoring, memory reconciliation, connection checks, and recovery when the tool contract alone is insufficient.
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

## Task and memory details

Match an existing Task by objective and scope, not title, recency, or candidate
count. Its Objective states the requested outcome and constraints; Current
position records the approach, reasons, evidence, progress, and blockers;
Resume point gives the next concrete action or unresolved question; References
points to useful IDs and files without hiding essential context outside the Task.
Evolve the full current body rather than appending a log or copying a Plan.
Keep unresolved issues until resolved. Completed or abandoned work leaves the
working set; blocked work stays active with its resume condition.

Choose `create`, `evolve`, or `consolidate` explicitly and copy returned IDs and
versions. Select user/project/machine scope deliberately. A one-off request is
not a lasting preference; a small reusable correction can deserve a memory
without a Task. Label proposals and unverified claims. For multiple candidates,
use the current request to select relevant ones; ask only when ambiguity changes
the work. Follow the tool contract for recall timing. Do
not use `history` or another recall merely to reconfirm a successful save.
Repeated friction may warrant a learned procedure through the existing
`remember_this` authoring flow. Follow returned instructions, not an invented
tool or action.

## Resolve, do not conceal

A retained proposal is not published. Continue review through `remember_this` using
returned references; do not resubmit unchanged proposals. Ask the user to resolve
conflicting standing rules or decisions requiring their choice.

If required context is unavailable, report it and pause dependent changes. Lexical-only
success is not semantic readiness. Use CLI `doctor` when needed; repair only within
authorization. Git sharing is separate from saving and requires sensitive-pattern
inspection and the appropriate user decision.
