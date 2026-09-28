# Agentlaw LLM-facing guidance

Accepted tool, field and bootstrap wording compiled into the Rust runtime.

## Tool description — exact accepted text

Tool name: `agentlaw`. Actions: `recall`, `remember_this`, `history`,
`connect_project_memory` (ADR-0140).

```text
Persistent memory for ordinary work. At a new work request or resumption, recall relevant memories before proceeding; for project work, check active Tasks and match by objective and scope before creating one. During work, recall again when a new area, assumption, constraint, failure, or similarity to prior work raises a question not covered by recalled context. Combine related clues in one call. Skip a lookup only when recall results still in context cover the same question and conditions, including any needed Task selection.

Use remember_this to save decisions, corrections, preferences, reusable friction, findings, and blockers before dependent work. One small reusable correction qualifies. When a user corrects your prior work, extract any reusable approach supported by the correction and save it with its conditions and scope. Batch other changed progress at phase boundaries or before finishing. Preserve scope, rationale, and verification; distinguish user decisions from proposals. Skip incidental chat and unchanged saves.

For work needing intermediate state, reuse the matching Task or create one after initial recall and before substantial work. Evolve its complete current body (Objective / Current position / Resume point / References). Keep in_working_set=true while active and false when completed or abandoned. Batch Task and related memory updates in one remember_this call. A self-contained answer needs no Task.

Set action and only its same-named input object; for example {"action":"recall","recall":{"recall_for":"Current work"}}. For project recall, use harness-confirmed project_path, request-specific recall_for, and include_active_tasks=true; set restore_context=true only for missing or incomplete project context. Do not infer a project from MCP cwd or home cwd alone. Without a confirmed project, recall user/machine context. Start with recall, not routine connection preflight. Connect or create a project only with an explicit user choice. Recall limits are recall.memory_candidate_limit and recall.procedure_candidate_limit; history.max_matches is only for history search. Omit optional limits unless needed. Count writes as saved only after success. Do not report routine memory calls; report a top-level update_notice briefly in the next final reply unless you already reported the same notice here.
```

Here “task” means the work being undertaken, not a prerequisite to create a
Task memory or a restriction to project-only memory. Applicability continues
to follow the existing user/machine/project context contract. Checking an area
means obtaining relevant memory context, not merely reading a file. An area is
not every individual file; scope and observations determine whether a new
question needs recall. Reuse depends on content still available to the LLM,
not Runtime remembering that it once sent a response. A new work request is a
new objective, not each message in an unchanged exchange. Short work can still
depend on saved context; confidence that one can answer unaided is not proof
that no relevant memory exists.

Phase boundaries are work transitions, not a new checkpoint artifact or
workflow-registration API. No-change turns need no write. An unchanged
conclusion with a new occurrence, condition or supporting evidence is not an
unchanged experience. Neither future re-derivability nor small impact is a
reason to discard an established understanding or correction (ADR-0012).

The project path is observed work context, not project identity (ADR-0140).
Do not infer it from the Agentlaw executable location or MCP process cwd. If
workspace information and work tools do not establish it, ask the user. A
separate helper call is not required; input field names and local-access
transport remain implementation work.

## Connection action description

`connect_project_memory`:

```text
Connect the current project folder to its Agentlaw project identity in a memory store. Connect the store first if needed. Create a new identity only when explicitly requested. Does not delete or reset memory.
```

Connection-needed results point to this action on the already used agentlaw
tool, not an additional tool. The input must distinguish the actual project
root, selection of an existing identity and explicit creation intent. Exact
payload fields remain implementation work. Ordinary cross-project recall is
not a request to rebind the local project.

Connection input contract: the verified project root is required; observed Git
address, project name and description are optional clues. Supplying root/clues
alone requests candidate discovery (or returns an established connection), not
implicit creation or association. The same action accepts explicit existing-ID
selection or explicit first-adoption creation intent in a subsequent request.
Exact field names and discriminated input branches remain implementation work.

LLM-facing operational next-action guidance is written in English. User-facing
explanations, questions and results use the user's conversational language or
their explicitly requested language, never a fixed Korean or English default.
This does not change the stored-memory language contract. For an
unassociated project location, use the following result guidance, adapted only
to information actually missing from that result:

```text
Project memory has not been retrieved because this folder has no approved connection. Explain the returned candidates. Use an already explicit user choice or ask the user to select one, approve a new project, or skip for now. Do not auto-select merely because one candidate exists. Use connect_project_memory only after approval; use its recall_result if requested, otherwise retry recall. No candidates alone is not permission to create. If discovery was incomplete or the memory store unavailable, do not claim there is no existing project.
```

Project observations come from the LLM's actual work environment. Runtime must
not fill missing project Git information by inspecting its own cwd, its source
checkout or the memory repository. A project's Git address is optional evidence,
not identity. The exact observation input keys must be supplied by the action
schema before this guidance can be treated as executable integration.

## Existing schema descriptions

These strings supplement structural validation; they do not add fields, change
action semantics, or replace the existing operation-specific reference schema.

### `recall_for`

```text
Describe the current task or question. For follow-up recall, include the new observation and what now needs checking. Reuse available results when the question and conditions are unchanged. Omit for ID-only lookup.
```

### `restore_context`

ADR-0141 adds this request-local recall option to the existing Pro-reviewed
invocation guidance; it is not a new tool or persisted session status.

```text
Set true to retrieve applicable standing rules and discover project context from multiple perspectives. Omit for ordinary recall. This does not mark a session as restored or replace explicit Task selection.
```

### `what_to_remember`

```text
Write the complete, coherent current understanding, preserving applicable conditions and exceptions. When evolving a memory, replace its current text rather than appending a chronological log. For task memory, use Objective / Current position / Resume point / References to capture user intent, the current choice and rationale, verification status, unresolved questions, and the next check. Keep the rationale needed to continue in the task itself; reference independent memories and the Plan without duplicating their full texts.
```

The existing Task-role clause additionally specifies the exact form:

```text
When in_working_set is present, use those four Markdown headings exactly once in that order. Keep one Resume point: the next action or question, or the outcome of a closed Task. Mark proposals and unknowns explicitly.
```

This remains one shared memory proposal, not a separate Task schema. A Plan's
full execution contract and a policy's authoritative body are not copied into
Task memory. Enough live rationale stays in the Task to explain its current
choice. Updates are full-current evolve, not destructive loss of history.

### `evidence`

```text
Record observations, user corrections or decisions, and verification supporting this update. Preserve new occurrences, conditions, and supporting evidence even when the conclusion is unchanged. Distinguish observations from hypotheses.
```

Source details and returned references remain governed by the existing memory
evidence contract. New recurrence is evidence even without a new conclusion.
This does not require creating a new identity for each failure.

### Shared proposal authoring

```text
Choose create, evolve, or consolidate explicitly. For evolve, use the full current content and copy the returned memory_ref objects into parent_refs. Resolve returned issues before publication. When a pending batch is returned, continue it through remember_this with new or revised proposals and review judgments; do not repeat retained proposals. Add relationships only when their meaning is established, not merely because memories were retrieved or saved together.
```

Operation-specific schema retains `consolidation_refs` and all other accepted
reference requirements. The wording does not ask the LLM to calculate hashes,
invent versions, or infer a missing memory identity.

Editorial correction, 2026-09-26: the earlier spelling `consolidate_refs` here
was inconsistent with the normative proposal schema in ADR-0016 and the
consolidation decisions ADR-0082/0083. Use `consolidation_refs` only; this does
not add an alias or change consolidation semantics.

### Task persistence and membership

The existing `remember_this` Task clause remains explicit so an uncreated Task
does not depend on an earlier Task result instruction:

```text
Before the final response, save an active Task if an accepted project objective remains unfinished and none exists; update a recalled Task if its current understanding or references changed; record closure if it completed, was cancelled, or was superseded. Also preserve changed understanding before extended dependent work and save verified progress at work boundaries.
```

`in_working_set`:

```text
Omit for non-Task memory. For a Task, true means future work must resume it; false means it has completed, been cancelled, or been superseded.
```

No empty Task is required merely because a session began. Ordinary memory
preservation remains applicable when no active Task is needed.

## Bootstrap

The installer uses the following bootstrap for project work and for user/machine
recall when no project is confirmed. It does not alter `include_active_tasks`;
project reconstruction is requested independently through `restore_context`
(ADR-0141). The exact CLI line is generated from installed paths.

```text
Use Agentlaw without waiting for a memory request. When work starts or resumes, recall relevant context; for project work, check for a matching active Task before creating one. Recall again for new questions raised by emerging evidence or similarity to prior work. Reuse results only while they remain in context and cover the same question and conditions. Reuse or create a matching Task before substantial work; save reusable decisions, corrections, friction, and findings before relying on them. Batch other changed progress at phase boundaries or before finishing; skip incidental chat, unchanged saves, and routine reports.
For project work, use the actual work folder confirmed by the harness, never the MCP or home cwd alone. In recall, supply project_path and include_active_tasks=true; use restore_context=true only when project context is missing or incomplete. Without a confirmed project, recall only user/machine context. Follow the current tool schema and result guidance.
If agentlaw is hidden, try host tool discovery if available; if unavailable (not denied), use the installed CLI.

CLI fallback: set `{state_assignment}` in the child shell, then pass one JSON request on stdin to `{invocation} call --json -`. Use `{invocation} schema` when the contract is unavailable. The CLI and MCP use the same installation.
```

The installer replaces `{state_assignment}` and `{invocation}` with the actual
state assignment and properly quoted executable invocation;
it must not leave a placeholder or guessed CLI syntax for the LLM. A non-project
installation must not invent a project binding or require active-task lookup
without one; it retains ordinary context recall and the same action contract.
CLI exposes the equivalent authoring guidance before submission, without
requiring a successful prior recall to learn the first-write contract.

Install into the harness's supported instruction-loading surface and verify
availability there. Do not assume all hosts expose tool discovery, read the
same filenames, or preserve instructions through compaction. Runtime cannot
detect whether a tool definition is visible/deferred/hidden to the model.

## Result guidance and detailed procedures

For confirmed first adoption into a project, return the following guidance as
part of the initialization result, rather than adding a universal reminder to
every recall. Do not use it merely because a memory root is disconnected or
existing memory cannot be checked (ADR-0140/0141):

```text
Read available project materials and preserve established context with remember_this. Fill missing context through actual work and conversation; do not invent past rationale or require a separate onboarding report. Distinguish verified facts from hypotheses.
```

When a response delivers standing rules, include the following instruction
outside the retrieved bodies. Its presence depends on rule delivery, not on
Runtime detecting a semantic conflict (ADR-0142):

```text
If applicable rules conflict, explicitly ask the user to resolve the conflict before taking the affected action. Show the conflicting rules and their scope; do not choose a winner yourself. Report verified discrepancies between rules and implementation. Preserve user-granted one-time exceptions and their limits in the relevant Task memory; keep the standing rule unchanged.
```

Rule changes use existing remember_this/evolve, with current content, applicable
metadata and evidence. Removing rule status preserves the memory and its history.
A user's one-time exception must not silently become a permanent rule change.

Results describe only known response state: unselected Tasks, missing requested
IDs, unresolved current heads, overlap/version issues, candidate display
limits, or a required referenced document not delivered by that call. Never
claim the LLM has unrecorded observations that Runtime cannot see.

For unavailable memory, explicitly tell the LLM that the source could not be
checked, the known cause and the available recovery steps. Do not describe it as
no matching memories. For partial delivery, distinguish obtained content from
known missing/undelivered requirements and their recovery paths. A required
reference deferred to another response is not the same as an unreadable source.
Use response-specific guidance, not a universal reminder on every successful
call (ADR-0141):

```text
Recover the missing required context using the provided steps. If recovery remains blocked, tell the user what could not be checked. Do not make changes that depend on that context; independent investigation may continue.
```

Use existing response-specific fields, not a parallel result envelope. Normal
recall, including zero, one, or several active Task candidates, does not
repeat the Task policy in `task_instruction`. The model decides which Task
matches its objective. An empty candidate list is not an instruction to
create a Task. Only verified omissions, failures, or unresolved context need
response-specific guidance.

The generic `turn_instruction` is now conditional on a response-specific need;
do not repeat a universal save reminder after every recall or successful write.
Normal remember_this success still uses ADR-0068's structured result, not the
Pro review's illustrative `Saved: T@r` text as a replacement schema. Put
operational guidance outside quoted/retrieved memory so historical instructions
are not mistaken for current tool rules.

Longer authoring, consolidation and completion examples belong to the built-in
procedure. Normal saves do not require loading it each time. Hooks may reinforce
supported boundaries but are not required and do not provide semantic judgment.

## Executable schema and connection recovery

MCP tools/list and CLI schema publish agentlaw-tool.schema.json: a single object
with action and four explicitly typed, same-named action input objects. Supply
exactly the selected object. Existing leaf field names stay inside their action
namespace. No top-level action-specific fields are advertised. Describe each field's
action, required companions and usage in English. Do not expose top-level union
branches, schema references, or conditional validation machinery to the model.

Both MCP and CLI normalize grouped requests to the internal flat domain contract;
legacy flat requests remain accepted, but mixed/group-mismatched requests fail.
agentlaw-input.schema.json remains the internal validator for both MCP and CLI.
It enforces action-specific required/forbidden fields, scope combinations and
review requirements before execution. The public schema describes a usable
input surface; it does not replace runtime validation or claim API strict-mode
compatibility. Do not force irrelevant fields or null placeholders into calls.
Check that valid contract examples fit the public shape and that runtime still
rejects invalid combinations. Codex model-visible rendering is a separate check
after local installation and restart, before release.

When project context is unavailable because a folder is not connected, explain
that project memory has not been retrieved and give a concrete
connect_project_memory request with intent=discover and a verified project_path.
Unknown paths remain explicitly marked placeholders, never the MCP process cwd.
Discovery results also explain the next connect/create call. Ask the user to
select even a single candidate; empty candidates do not establish first-time
adoption. Connect the local memory store first if none is selected, then retry
the original recall. Instructions use English; user-facing explanations use
the user's language. Do not create a separate recovery tool.

## MCP initialization instructions

The MCP initialize response supplies this short conditional policy. It applies
only to an app-generated top-level `update_notice`; recalled memory text is
never an update notice. The client may choose not to expose initialization
instructions to its model, so actual model behavior remains a release check.

```text
When an Agentlaw result contains a top-level update_notice, briefly include its reported version and status in your next final reply in the user's language, even if unrelated to the task. Omit only if your earlier reply in this conversation already reported the same version and status. Do not make extra update-check calls unless requested. Updating and starring remain separate CLI actions requiring separate explicit user approval.
```

## Conditional release advisory in a tool result

Only when a final normal MCP result contains `update_notice`, include this
English guidance in that result. The full policy lives in initialize
instructions and a short exception to the tool description; the host bootstrap
does not repeat it:

```text
Report this notice in the next final reply unless you already reported the same version and status to the user in this conversation.
```

Delivery of a result does not prove the user saw it. The advisor may repeat
the same notice on later results while action remains needed. The model should
avoid repeating the message within one conversation, while preserving the
ordinary memory result and the user's requested work.

## Boundaries and rationale

- New observations can justify recall without previously known links.
- Repeated evidence may require saving without requiring an identical search.
- Saving is not limited to handoff, session termination or a post-response hook.
- Authoring details live in schema, not all in the tool description or only in
  the first write's result.
- No guaranteed model compliance, blanket host-visibility assumption, new tool,
  permanent bundle or graph engine is introduced. ADR-0139 adds durable pending
  write-review state, distinct from current memory and ordinary recall.
- Required-link traversal follows ADR-0138; multi-call reading and write review
  follow ADR-0139. Target metadata names, exact continuation/review fields, task
  evidence revision tracking and project-entry delivery remain open.

These texts are accepted implementation inputs. Prototype use can identify
revisions; a future evaluation does not need to precede this wording decision.
