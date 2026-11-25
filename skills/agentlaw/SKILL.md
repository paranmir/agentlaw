---
name: agentlaw
description: Use for detailed Agentlaw Task authoring, memory reconciliation, connection checks, and recovery when the tool contract alone is insufficient.
---

# Agentlaw

Use the installed tool's contract for requests, recall and saving. Judge which memories apply.

## Check a connection

When asked about connectivity, run a recall for the verified project and inspect
its diagnostics. Empty success means the call returned no matches. Never write a
dummy memory. Report the folder, what the recall accessed, and any degraded or
unverified capability. Without a verified project, say its connection is unchecked.

If MCP is missing, try tool discovery, then the installed CLI with its configured
state directory and `agentlaw schema`. CLI success verifies only the CLI path.
For an unavailable store, follow recovery guidance using a local Markdown store
path. A GitHub URL cannot serve as that path. Confirm creation and location if a
new store is needed. Preserve existing state.

Reuse a folder's binding across sessions. For an unbound folder, use the candidates
already returned and the contract's connect/create guidance. Run `discover` only
when candidates are missing or need refreshing.

## Write a resumable Task

Request active Tasks at project start, resumption, recovery after compaction, or when selecting a Task. Omit the option for ordinary follow-up questions. Before editing a Task from its handoff or reconciling heads, read every full current body and reference by ID or from the immediately preceding conflict response's `current_states`.

Check the candidate's objective and scope even when the count, title, recency or rank
suggests a match. Ask only if ambiguity affects the work. Fill the four headings
so a fresh session can act:

- **Objective** — requested outcome and constraints.
- **Current position** — approach, rationale, progress, blockers and unresolved
  issues. Include consequential failed or rejected attempts. Identify the project,
  revision and coverage of verification. Distinguish observations from hypotheses.
- **Resume point** — next safe action or open question. Keep a blocked Task active
  and name the condition for resuming. For an in-flight operation, record the
  request, known status and how to check it before retrying.
- **References** — durable IDs, files and Plan. Keep essential chat-only reasons
  in the Task itself.

## Reconcile and recover

Mark a rule only for explicitly established standing instructions. Keep the action,
conditions and lasting exceptions in its body, the reasons in `evidence`, and
one-off exceptions in the Task. Installation versions and measurements belong
in ordinary memory. Use the tool's `is_rule` guidance when applying rules that overlap or conflict.

When changing text, rewrite it into its current state. For a metadata or relationship
change on a single current head, omit the unchanged body and supply its reference
and evidence. Read by ID if you need the reference.

Ask the user to decide when an unresolved choice is theirs. For Git sharing, use the applicable sensitive-pattern check and obtain the appropriate user decision.

A confirmed save needs no verification recall. After an uncertain write, verify
the published content and reference before repeating it. Lexical results alone
do not establish semantic readiness. For a recall integrity error (`source_corrupt`), preserve the bytes and check
`agentlaw doctor --audit` before recovery. If the local spool fails, repeat the recall for a fresh copy. For an unreadable local epoch, check the binding’s local state. Follow a nested `partial_recall_error` while retaining the unbound folder’s connection choices. Use the failing operation’s own guidance and repair
only within authorization. Recurring friction may warrant a learned procedure
through `remember_this`. These procedures are saved instructions, distinct from
installed harness skills.
