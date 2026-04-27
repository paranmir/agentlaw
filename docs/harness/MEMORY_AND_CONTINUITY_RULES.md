# Memory And Continuity Rules

## Purpose
This document defines how Harness memory and continuity operate as default Harness subsystems without becoming governing law.

It owns operational memory rules. It does not own evaluation methodology, benchmark design, or product-specific memory facts.

## Default-On Rule
Harness Memory is part of the default Harness structure.

Every Harness installation should treat the following as default subsystems:
- canonical memory files under `memory/*`
- exact lookup, FTS, and vector indexes co-located in `.harness/index/meta.db` (vector via the `sqlite-vec` extension; no separate vector directory)
- embedding model artifacts under `.harness/models/embedding/<model-name>/`
- runtime job and cache paths under `.harness/jobs/` and `.harness/cache/`
- the bundled MCP server (shipped with the `rule-harness` pip package) as the LLM's only access path to indexed memory; raw SQL access is prohibited
- session restore behavior that can start, validate, repair, or degrade memory runtime components

Default subsystem status does not require every runtime process to stay hot. Runtime components may be idle, missing, repairing, or degraded as long as the canonical memory layer remains inspectable and the failure is handled as lower-authority runtime state.

## Canonical Memory Operation Path
Memory operations are canonical when they go through the bundled Harness Memory MCP tools. CLI fallback surfaces (`rule-harness session-restore`, `rule-harness memory-runtime-check`, `rule-harness memory-runtime-repair`, `rule-harness mcp-recover`) are explicit recovery and diagnostic entry points for sessions where MCP is not available. They are not parallel memory authorities.

### Session Path Disclosure
A session that performs memory read or write operations without the Harness Memory MCP registered is operating in a **degraded memory session**. Degraded status must be visible, not hidden:

- Every restore (MCP or fallback) must state the path it used. MCP restores state `via MCP`. Fallback restores state `via recovery fallback (MCP unavailable)` and surface the degraded status in the summary, not only in runtime internals.
- Before initiating any memory-write operation, the agent must declare which path it will use. "Declare" means name the specific tool or command about to run (for example, `memory_save_item`, `harness_session_save`, or `rule-harness mcp-recover`), so that accidental fallback writes are visible before the write happens, not after.
- If the declared path is a fallback surface, the session is degraded and the write is subject to the recovery-only rules below.

### Fallback Self-Labeling
CLI fallback commands must carry a visible recovery or degraded-mode label in their output, in both machine-readable and human-readable forms. Consumers must be able to tell a fallback response apart from an MCP response without comparing shapes. Specific field names are an implementation detail; the requirement is that the label is present and obvious.

### Write-Path Boundary
The only sanctioned memory-write paths are:

1. The MCP write tools (`memory_save_item`, `memory_append_log`, `memory_set_status`, `memory_supersede`, `memory_propose_promotion`, `harness_session_save`).
2. `memory_runtime_repair` / `memory_runtime_repair_start` (from canonical Markdown; these repair derived state, not canonical content).

Direct edits to files under `memory/*` outside these paths are governance drift unless paired with an explicit `memory_runtime_repair` invocation in the same change, so that derived DB, chunk, FTS, and vector state is not left silently out of sync with canonical Markdown. The pairing must be visible in the change record (plan, commit, or save log), not assumed.

### Recovery Loop Expectation
A session that finds itself in degraded mode should prefer resolving the underlying cause (MCP registration, runtime repair, new agent session) over routing work through fallback commands. Fallback commands exist to make the degraded state diagnosable and recoverable, not to become a second memory API.

## Authority Boundary
Memory is always below law.

Memory may:
- restore session context
- recall source-linked repository state
- preserve user preferences
- preserve recent operational history
- assemble compact context packets
- propose recursive-improvement candidates

Memory must not:
- override the constitution, root control documents, or `docs/harness/*`
- silently promote preferences into law
- replace plans, references, generated facts, or git history when durable reviewable records are needed
- treat SQL, vector indexes, caches, or MCP outputs as governing authority
- promote itself directly into law, skills, plans, references, or shared artifacts

## Canonical Layout
The canonical memory layer is file-based and human-reviewable:

```text
memory/
  known-facts/
  logs/
  rules/
  preferences.md
  working-set.md
  LOOKUP_RULES.md
```

Derived runtime and index state belongs under `.harness/`:

```text
.harness/
  index/
    meta.db                # SQLite + FTS5 + sqlite-vec virtual tables
  models/
    embedding/
      <model-name>/        # downloaded by rule-harness init
  jobs/
  cache/
```

`memory/*` is the source layer. `.harness/*` is derived runtime or index state and may be repaired from source material.

The current default embedding model is `intfloat/multilingual-e5-small` (384-dim, multilingual). The `sqlite-vec` extension requires `sqlite3.enable_load_extension(True)` in the host Python binding; the `rule-harness` pip package must verify this capability at install and surface a clear error if disabled.

## Memory Types
`memory/known-facts/` stores currently remembered facts. Known facts are memory records, not governing rules. Each known fact must carry source, status, and verification metadata once the format is defined.

`memory/logs/` stores historical events. Logs are append-oriented. Incorrect past conclusions should be corrected by later log entries rather than erased by default.

`memory/rules/` stores project-local governance rules. Rules are distinct from known facts: facts are remembered state, rules are local law-level policy for the repository's own authoring workflow. Rules sit below shared `docs/harness/*` in authority and above known facts and preferences. Full framework in the "Local Rule Memory Type" section below.

`memory/preferences.md` stores user or maintainer preferences. Preferences may guide style and workflow, but they lose to law, current explicit instruction, and safety or verification requirements.

`memory/working-set.md` stores current active context for low-token session continuity. It must be refreshed when active plans, current decisions, or next actions change. The working-set file is rewritten in full on every `harness_session_save`; bullets inside the file are not separately addressable as item-level records and are not declared as a memory item type.

The declared item types are exactly three: `fact`, `preference`, and `rule`. Each type has a defined write path under `memory_save_item`. No other types are declared; calls with any other `type` value return `memory.invalid_params`.

`memory/LOOKUP_RULES.md` stores retrieval policy. It tells agents and tools how to choose memory sources; it does not create higher authority than the memory layer.

## Local Rule Memory Type
Rules are project-local governance items: persistent policy that applies within the repository's own authoring workflow without rising to shared-law authority. The framework defined here is general and ships to every target project via `publish-repo/`; rule content is project-specific and must never leak into the distribution seed.

### Purpose And Distinction From Facts
- A known fact is a remembered observation about current state (for example, "the current active plan is X"). It loses to new observation.
- A rule is a standing policy about how work is done inside this repository (for example, "authoring-workspace distribution topology must be verified before release"). It loses only to higher authority, not to new observation.
- A log records that an event happened. A rule says what should happen.

The three types are stored separately so that status transitions, retrieval treatment, and write-path obligations can differ without role-mixing.

### Authority Position
Rules sit strictly below shared `docs/harness/*` and root control documents, and strictly above known facts, preferences, logs, and working-set entries. Conflicts resolve as follows:

- Shared law vs. local rule → shared law wins. The rule must be updated or marked `superseded` to align.
- Local rule vs. known fact → the rule wins. A known fact that contradicts an active rule must be reconciled (fact becomes `stale`, or the rule is revised).
- Local rule vs. preference → the rule wins.
- Local rule vs. working set → the rule wins; the working set must not assert content that contradicts an active rule.
- Two active rules in the same scope in genuine conflict → governance drift. Resolve through `HARNESS_FIX_TOOL.md`, not by silent status change.

Rules do not override current explicit user instruction within a single session, but they do survive it: an agent that takes a one-off instruction contradicting an active rule must surface the conflict rather than quietly comply.

### Front Matter Schema
Every rule file under `memory/rules/` must carry the following front matter:

```yaml
---
id: rule/<kebab-case-slug>
type: rule
status: active            # follows memory status values (active, stale, superseded, ...)
scope: repository         # allowed: global | repository | user | session
summary: <one-line what-this-rule-says>
applies_when: [always]    # list of opaque string tags; `always` is the default
source:
  - path: <pointer to originating artifact when applicable>
    relation: derived_from | supersedes | discussed_in
last_checked: YYYY-MM-DD
supersedes: []            # optional list of prior rule ids
tags: []                  # optional free-form tags
---
```

Required keys: `id`, `type`, `status`, `scope`, `summary`, `applies_when`, `source`, `last_checked`. Trigger tags inside `applies_when` are opaque strings matched as exact literals; shared law does not enumerate a vocabulary beyond the sentinel `always`. Each project owns its own trigger vocabulary inside its own rule bodies.

### File Layout
One rule per file under `memory/rules/`. File names are lowercase kebab-case and describe the rule's scope or subject:

```text
memory/rules/authoring-workspace-operations.md
memory/rules/release-readiness-gate.md
```

### Discoverability Obligations
- **Session restore**: `harness_session_restore` must include active rules matching the packet's scope under an `active_rules` field (see "Session Restore Packet Format" below).
- **Lookup read order**: `memory/LOOKUP_RULES.md` must consult rules before known facts when both are relevant. Active rules with `applies_when: [always]` are always in-scope for the current session.
- **Trigger matching**: non-`always` tags are matched as exact strings against the current trigger context. Law does not define fuzzy matching, hierarchy, or wildcards for trigger tags. A project that needs a taxonomy defines it inside its own rule bodies.

### Write Path
Rules are created, updated, and lifecycled through the same MCP tools that handle facts, with `type="rule"` as the discriminator:

- **Create / update**: `memory_save_item(type="rule", id=..., front_matter=..., body=...)`.
- **List**: `memory_list(type="rule", scope=..., status=..., applies_when=...)`.
- **Search**: `memory_search(..., types=["rule", ...])` — rules rank alongside facts by the same FTS+vector score; no authority-mixing in rank.
- **Status transition**: `memory_set_status`, `memory_supersede`. These follow the same conflict rules as facts.
- **Promotion**: a rule that has grown into shared-law territory is promoted through `memory_propose_promotion`, not rewritten in place.

Direct file edits under `memory/rules/` are subject to the same Write-Path Boundary as other memory layers: they are governance drift unless paired with an explicit `memory_runtime_repair` invocation in the same change.

### Distribution Boundary
`publish-repo/memory/rules/` must not contain this authoring workspace's local rule content. Target projects that install via the `rule-harness` pip package or `rule-harness init` inherit an empty rules directory (with at most a one-paragraph starter README explaining the directory's purpose). They populate their own rules locally. The `rule-harness verify` publish-seed leak detector scans this path for authoring-specific marker phrases and fails closed on leaks.

### Schema Storage Note
The `memory_items.type` column in `.harness/index/meta.db` is constrained by a CHECK that admits `fact`, `preference`, `rule`, and `working_set_entry`. The runtime tool layer accepts only the first three; calls to `memory_save_item(type="working_set_entry")` are rejected with `memory.invalid_params` before they reach the DB, and the working-set file is rewritten in full by `harness_session_save` rather than addressed at item-level. The CHECK retains the fourth value so that databases written by older binaries continue to read; rows of that type are dormant and not produced by any current write path.

## Canonical File Formats
Canonical memory files use Markdown with YAML front matter when item-level metadata is needed.

Markdown keeps memory human-reviewable. YAML front matter keeps indexes and agents from scraping prose for authority, status, scope, and source data.

### Known Fact File
Known facts should use one fact per file under `memory/known-facts/`.

File names should be lowercase kebab case and describe the remembered fact:

```text
memory/known-facts/current-active-plan.md
memory/known-facts/test-command.md
```

Minimum format:

```markdown
---
id: fact/current-active-plan
status: active
scope: repository
source:
  - path: docs/plans/active/harness-default-memory-distribution-revision-plan.md
    relation: derived_from
last_checked: 2026-04-20
confidence: high
supersedes: []
tags: [planning, memory]
---

# Current Active Plan

The current active memory distribution driver is `docs/plans/active/harness-default-memory-distribution-revision-plan.md`.

## Notes

- This is a remembered current fact, not law.
```

Required front matter:
- `id`
- `status`
- `scope`
- `source`
- `last_checked`
- `confidence`

Allowed `status` values are defined in this document's conflict rules.

Known facts must not be used for ordinary retrieval when `status` is `stale`, `superseded`, `disputed`, or `quarantined`.

### Rule File
Rules use one rule per file under `memory/rules/`. File names are lowercase kebab-case and describe the rule's subject:

```text
memory/rules/authoring-workspace-operations.md
memory/rules/release-readiness-gate.md
```

Minimum format:

```markdown
---
id: rule/authoring-workspace-operations
type: rule
status: active
scope: repository
summary: Authoring workspace operations contract for this repository.
applies_when: [always]
source:
  - path: docs/plans/completed/2026-04-24-local-rule-memory-type-plan.md
    relation: derived_from
last_checked: 2026-04-24
supersedes: []
tags: [authoring, operations]
---

# Authoring Workspace Operations

Body describes the rule in prose. Keep it short enough that a session-
restore consumer can read the file without expanding a separate budget.

## Notes

- This is a project-local rule, not shared law.
- It survives status updates until explicitly superseded.
```

Required front matter: `id`, `type`, `status`, `scope`, `summary`, `applies_when`, `source`, `last_checked`. See the "Local Rule Memory Type" section for the authority position, discoverability obligations, and write-path contract.

Rules must not be used for ordinary retrieval when `status` is `stale`, `superseded`, `disputed`, or `quarantined`. A rule that conflicts with shared law must be updated or superseded — it must not remain `active` once the conflict is surfaced.

### Log File
Logs should be date- or session-grouped under `memory/logs/`.

Recommended path:

```text
memory/logs/YYYY-MM/YYYY-MM-DD.md
```

Minimum entry format:

```markdown
## 2026-04-20T14:30:00+09:00 - always-on memory decision

- id: log/2026-04-20/always-on-memory-decision
- kind: decision
- scope: repository
- status: recorded
- source:
  - conversation
  - docs/harness/MEMORY_AND_CONTINUITY_RULES.md

Harness Memory was accepted as a default Harness subsystem. This log is historical and must not be retrieved as current truth without checking current law and active plans.
```

Logs may preserve obsolete information, but retrieval must not treat old log conclusions as current facts unless an active known fact or current source confirms them.

### Preferences File
Preferences live in `memory/preferences.md`.

Minimum section format:

```markdown
## concise-final-answers

- id: preference/concise-final-answers
- status: active
- scope: user
- source: conversation
- last_checked: 2026-04-20
- applies_to: final_response
- loses_to: [law, current_explicit_instruction, safety, verification]

The user prefers concise final answers that emphasize completed work, verification, and next steps.
```

Preferences may remain active globally while being `suppressed` for a specific request.

#### Machine-Writable Grammar

The `memory_save_item(type="preference", ...)` write path edits `memory/preferences.md` in place using section-aware editing. The grammar that the writer recognizes:

- **Heading level**: each preference is a single `## <slug>` section. No `###` subsections, no nested headings inside a preference.
- **Slug format**: lowercase kebab-case. The slug equals the suffix of the preference id (`preference/<slug>` → `<slug>`). Same-slug duplicate sections are not allowed; a second write to the same id replaces the existing section.
- **Required field list lines** (each on its own line, in this order, immediately after the heading and one blank line): `- id:`, `- status:`, `- scope:`, `- source:`, `- last_checked:`, `- applies_to:`, `- loses_to:`. Optional additional fields may follow the required ones; the writer treats them as opaque passthrough.
- **Body terminator**: a section's body extends from after the field list to the next `## ` heading or end of file. The writer rewrites only the target section's bytes; sibling sections must round-trip byte-identical.
- **File-level front matter**: an optional YAML front matter block at the top of the file may carry file-level metadata (e.g., `updated_at`). Every section write refreshes the file-level `updated_at` timestamp; other front-matter fields are not touched.

Concurrent writes to canonical memory files are not protected; the kit assumes single-agent sequential operation. Tool clients are responsible for not issuing overlapping writes.

### Working Set File
The working set lives in `memory/working-set.md`.

It should be small enough to read during session restore without broad retrieval.

Minimum format:

```markdown
---
status: active
updated_at: 2026-04-20T14:30:00+09:00
scope: repository
---

# Working Set

## Current Goal
Define Harness Memory operational contracts before installer/runtime implementation.

## Active Plan
- `docs/plans/active/harness-default-memory-distribution-revision-plan.md`

## Current Decisions
- Memory is default-on.
- Canonical memory uses `memory/known-facts/`, `logs/`, `preferences.md`, `working-set.md`, and `LOOKUP_RULES.md`.

## Open Questions
- Installer/runtime design remains next.

## Next Actions
1. Define installer/runtime behavior after this contract is accepted.

## Authority Warnings
- Memory remains below law and approved artifacts.
```

The working set is current-context state, not a long-term changelog. It should be refreshed, compacted, or marked `stale` when active plans or current decisions change.

#### Working Set Field Discipline

The fields written through `harness_session_save` (`current_goal`, `active_plans`, `current_decisions`, `open_questions`, `next_actions`, `authority_warnings`) carry the **current snapshot only**. Each field is a short list. Each entry is one or two short lines describing a current state: a standing decision, an open question, a next action. The substantive narrative of what happened in this session — Finding / Evidence / Resolution prose, decision rationale, change history, file paths exhibited, evidence cited — does **not** belong in working-set fields.

That narrative belongs in the optional `log_entry` argument to `harness_session_save`. The argument is appended to `memory/logs/YYYY-MM/YYYY-MM-DD.md` and indexed for full-text and vector retrieval. Future agents searching for "what happened on 2026-04-26" or "why was Slice 7.5 added" use `memory_search` or `memory_recent_logs` against the log layer; they do not search the working-set, and the working-set is not indexed.

Concrete pattern:

- **`current_decisions` field, good shape**:
  ```
  - "Slice 7.5 added (Bootstrap & Update Architecture Cleanup)."
  - "working_set_entry dropped from declared types entirely (Path Drop)."
  - "Reminder fields renamed _review → _reminder for naming accuracy."
  ```
  Each entry: one short sentence, current standing decision, no rationale prose.

- **`current_decisions` field, anti-pattern**:
  ```
  - "Slice 7.5 added on 2026-04-26 after the user noticed the
     post-init Memory Intent reminder gap; tracing the gap led to a
     deeper finding that two parallel bootstrap routes coexist; the
     workflow reference is mostly only useful to the authoring repo
     developer; this slice retires the workflow reference pattern,
     consolidates the cycle inside the root tools, and inserts the
     Memory Intent reminder routing at the right point in the
     bootstrap flow ..."
  ```
  Multi-paragraph entry with full rationale belongs in the matching `log_entry.body`, not here. Every entry like this bloats the working-set by an order of magnitude.

The same discipline applies to every other working-set field. When in doubt: if the entry is longer than two short lines or contains words like "after", "because", "this slice", "evidence", it belongs in the `log_entry.body` and a one-line summary belongs in the working-set field.

This discipline matters because the working set is read at every `harness_session_restore`, the response packet's token cost scales with every additional character, and bloated fields slow every session start. The log layer absorbs as much narrative as needed without any of those costs because logs are indexed and retrievable, not always-loaded.

## `LOOKUP_RULES.md` Syntax
`memory/LOOKUP_RULES.md` defines retrieval policy with predictable Markdown sections.

Required sections:

```markdown
# Lookup Rules

## Read Order
1. `memory/working-set.md`
2. relevant active known facts
3. relevant preferences
4. recent logs only when current state is insufficient
5. source files referenced by selected memory

## Trigger Map
- session_restore: working-set, active known facts, recent decision logs
- user_preference: preferences, then source check if preference conflicts
- law_question: law files first, memory only as pointers
- implementation_question: working-set, active plan, known facts, then source files

## Budgets
- working_set_max_tokens: 1200
- memory_packet_max_tokens: 2500
- log_lookback_days_default: 14
- top_k_known_facts: 5
- top_k_logs: 5

## Exclusions
- exclude statuses: stale, superseded, disputed, quarantined
- exclude wrong scope unless explicitly requested
- exclude logs as current truth unless confirmed by active sources

## Escalation
- read law before memory when authority is unclear
- read source files when memory is tentative
- repair derived runtime search/vector state when source drift is detected
```

Rules:
- `LOOKUP_RULES.md` must not contain remembered facts.
- It may define trigger names, read order, budgets, exclusions, and escalation behavior.
- It must prefer small current context over broad history.
- It must require source reads when memory status or authority is uncertain.

## Session Restore Packet Format
A session restore operation should return a compact packet rather than dumping memory records.

Required packet fields:

```yaml
packet_type: harness_session_restore
generated_at: 2026-04-20T14:30:00+09:00
scope: repository
runtime_status:
  memory_service: running | idle | unavailable
  exact_index: ready | missing | rebuilding | stale
  vector_index: ready | missing | rebuilding | stale
authority_warnings:
  - Memory is lower authority than law and approved artifacts.
current_goal: string
active_plans:
  - path: plans/active/example.md
    status: active
last_decisions:
  - summary: string
    source: path-or-memory-id
    status: active
active_rules:
  - id: rule/authoring-workspace-operations
    scope: repository
    summary: string
    applies_when: [always]
    status: active
open_questions:
  - question: string
    source: path-or-memory-id
next_actions:
  - action: string
    source: path-or-memory-id
relevant_sources:
  - path: docs/harness/MEMORY_AND_CONTINUITY_RULES.md
    reason: owns memory operating rules
excluded_memory:
  - id: fact/obsolete-example
    reason: superseded
token_estimate:
  packet_tokens: 0
```

Rules:
- The packet must include source pointers for durable claims.
- The packet must identify excluded or suppressed memory when exclusion affects the task.
- The packet must stay compact and defer full source reads until needed.
- The packet must label runtime degradation rather than hiding it.
- The packet must not represent memory as current truth when status or authority is uncertain.
- The packet should include compact `authority_recall` guidance for memory subsystem work, artifact structure changes, and promotion decisions. This is runtime guidance only; it must not create durable "authority checked" records.
- The packet must state which path produced it (MCP or CLI fallback), so that consumers can recognize a degraded session at restore time rather than at first memory write. The agent's restore summary to the user must carry the same disclosure; see the Canonical Memory Operation Path section.
- The packet must include `active_rules` whenever active rules exist in the packet's scope. Rules with `applies_when: [always]` appear with id, scope, summary, `applies_when`, and status; rules with non-`always` triggers appear with the same compact shape — consumers expand a full body by id only when the trigger matches the current request. The field may be omitted or empty when no active rules are present.

## Markdown-Only Session Continuity
Before runtime services, indexes, or MCP tools exist, Harness session continuity uses the canonical Markdown layer directly.

The Markdown-only session path is:

```text
memory/working-set.md
memory/LOOKUP_RULES.md
memory/logs/YYYY-MM/YYYY-MM-DD.md
```

Rules:
- `memory/working-set.md` is the primary current session state.
- `memory/LOOKUP_RULES.md` tells agents how to restore context without broad reading.
- `memory/logs/*` records session decisions and handoff-relevant events.
- Session restore must start from `memory/working-set.md` when it exists.
- If `memory/working-set.md` is missing or stale, mark the restore as degraded and recover context from logs and source files.
- Session save must update `memory/working-set.md` first when the file exists.
- Session save should append a short session entry to `memory/logs/*` when the session produced durable decisions.

## Conflict Resolution
Memory conflict resolution follows:

```text
authority > scope > recency
```

Higher-authority repository files win over memory. Current scope beats wrong-scope memory. Recency is used only after authority and scope have been resolved.

Conflicts must be handled by status transition, not silent overwrite.

Allowed memory statuses are:
- `active`
- `tentative`
- `stale`
- `superseded`
- `disputed`
- `suppressed`
- `quarantined`

Retrieval treatment:
- `active`: normal retrieval allowed
- `tentative`: retrieval allowed only with verification warning
- `stale`: excluded from ordinary current-context retrieval
- `superseded`: excluded except when shown with the replacement source
- `disputed`: excluded until source conflict is checked
- `suppressed`: valid in general, but not applicable to the current request or authority context
- `quarantined`: ordinary retrieval prohibited until repaired

## Type-Specific Conflict Handling
Known facts that conflict with law, approved artifacts, or current repository files must not remain `active`.

Rules that conflict with shared law (`docs/harness/*`, `HARNESS_CONSTITUTION.md`) must not remain `active`. Revise the rule to align with shared law, or mark it `superseded` with a pointer to the law section that replaced it. Rules that conflict with an active known fact win the conflict (the rule is policy; the fact is observation) — the fact must be reconciled by status transition, not the rule.

Preferences that conflict with law or current explicit instruction should become `suppressed` for the current context rather than deleted by default.

Working-set entries that disagree with active plans or current repository state should become `stale` and be refreshed.

Logs remain historical records. A wrong or obsolete log conclusion should not be retrieved as current truth.

Indexes that disagree with canonical memory files or repository files are source drift. The canonical source wins and the index must be invalidated or repaired from canonical Markdown.

## Canonical Restore Route
Session restore must follow a fixed two-tier route. The mandatory tier is identical for every restore so harness behavior is deterministic across sessions, agents, and runtimes. The conditional tier expands only along pointers found in the mandatory tier.

When the bundled MCP server is available, the `harness_session_restore` tool implements this route end-to-end and returns the assembled packet. When the MCP server is unavailable, the agent must walk the same route manually against the canonical Markdown layer (degraded mode).

### Mandatory Tier
Every restore must perform these steps in order. The Mandatory Tier covers the body-level reads of every artifact whose substance the next response is likely to depend on, so that an agent following the procedure has a complete working picture before composing the first answer.

1. Run the workspace's harness verification command (or the equivalent integrity check exposed by the runtime). Record the result in the packet's `runtime_status`.
2. Read `memory/working-set.md` in full.
3. Read the body of every law file in full — `HARNESS_CONSTITUTION.md` and every file under `docs/harness/*`. The law layer is short and binding; rules that the session must apply must live in the agent's working context, not just as paths.
4. Read the body of every active plan — every file under `docs/plans/active/*`. Active plans are the work currently in flight; titles alone do not carry slices, contracts, or non-goals.
5. Read the body of the most recent `session_save` log entry — the single newest `memory/logs/YYYY-MM/YYYY-MM-DD.md` entry of `kind: session_save`. This is the directly preceding session's wrap-up summary, the most concentrated source of "what landed last" and "what is open right now".
6. List `memory/logs/*` entries dated within `log_lookback_days_default` (from `LOOKUP_RULES.md`, default 14 days) for the broader timeline. Titles only at this step — body reads for entries other than the most recent `session_save` (already covered by step 5) belong to the Conditional Tier.
7. Read the body of every entry in `active_rules`. Rule bodies live in `memory/rules/*` and must be read via the canonical memory source path; the packet's `active_rules` field carries only summary metadata, which is insufficient for applying the rule's full procedure or constraint.
8. Read `memory/preferences.md` in full. Preferences shape style and workflow choices the session should respect; the file is short, single-doc, and applies on every session.
9. Read `memory/LOOKUP_RULES.md` in full. The retrieval policy (read order, trigger map, budgets, exclusions, escalation) governs how the agent uses memory throughout the session and must live in the agent's working context, not be re-derived turn by turn.
10. Scan the known-facts manifest — collect the id, title, scope, and last_checked timestamp of every file under `memory/known-facts/*.md` whose status is `active`. **Bodies are not read at this step.** The manifest gives the agent a complete catalog of "what facts are remembered" so it does not invent or contradict known state; bodies are fetched on demand under step 11 or via working-frame search at step 11a.
11. Run a working-frame search against the indexed memory. Compose a query from `current_goal` plus `next_actions` plus `open_questions`, call `memory_search(query=<that text>, types=["fact"])` and surface the top hits with `id`, `title`, `path`, and snippet. The hits guide which known-facts the agent should fetch in full from the manifest assembled at step 10. The search is the layer's intended retrieval path; the manifest scan is the safety net that ensures nothing relevant is missed because of a query phrasing gap.
12. Detect governance drift: list `docs/harness/*` and `HARNESS_CONSTITUTION.md` files modified after the working set's `updated_at` timestamp. Flag any drift in `authority_warnings`.
13. Assemble the `harness_session_restore` packet (see "Session Restore Packet Format") including `authority_warnings`, the `stale-until-verified` flag, the source pointers gathered above, and the body fields produced by steps 3, 4, 5, 7, 8, 9, 10, and 11.
14. Surface gaps before composing a substantive response. If any of the above steps could not complete (missing file, ambiguous pointer, working-set referencing an artifact that no longer exists, rule body that fails to load, search engine unavailable), the agent must surface the gap to the user before answering. This applies the §Consult-Before-Answer Rule at session start: an honest "I cannot find what `current_goal` points at" beats a confident answer built on a partial restore.

### Conditional Tier
Expand only when the mandatory tier surfaces a concrete need:

15. Read a specific `memory/known-facts/*.md` entry's body only when the working-frame search at step 11 hit it, when `working-set.md` references it by id or path, or when the user explicitly asks for it. The manifest from step 10 plus the search hits from step 11 together drive this expansion.
16. Read a specific log entry body (other than the most recent `session_save`, which step 5 already covered) only when `working-set.md` references it or the user explicitly requests history.
17. Read source code files only when the request requires them and the working set does not already explain the relevant state.

### Degradation Rules
- If `memory/working-set.md` is missing or stale, mark the restore as `degraded` and recover context from logs and active plans.
- If the harness verification command cannot run, mark the restore as `degraded` and proceed without the integrity check, recording the omission.
- If runtime components (MCP server, indexes, embedding model) are missing or failing, degrade to the Markdown-only path. Do not treat missing runtime as higher authority than canonical Markdown.

### MCP Unavailable Recovery Route
When `harness-memory` MCP tools are not visible in a new agent session, agents must not assume memory is unavailable and must not silently bypass the memory subsystem. They must treat the condition as a startup, runtime, or agent-registration recovery problem.

The canonical recovery entry point is:

```text
rule-harness mcp-recover --target . --client auto --json
```

This command restores session context through the CLI fallback equivalent of the Canonical Restore Route, checks Harness runtime readiness, inspects agent MCP registration when the agent host exposes an inspection command, and returns concrete recovery actions.

Interpret the recovery result as follows:

- `harness_runtime.ok=false` means the Harness runtime or local memory store is not ready; repair the runtime before relying on indexed memory.
- `agent_registration.status=missing_registration` or `wrong_registration` means the agent host must be registered or updated through the agent setup path with explicit approval.
- `agent_registration.status=inspection_unavailable` means the host cannot be inspected from the current process; run the returned `manual_check` in the agent host shell.
- If registration is correct but MCP tools are still not visible, start a new agent session so the host respawns stdio MCP with the updated registration.

Recovery rules:

- `mcp-recover` is diagnostic and restore-oriented; it must not silently mutate agent configuration.
- MCP unavailable states should be resolved by startup, runtime, or registration recovery rather than by creating a parallel memory authority.
- Long-running memory runtime repair operations remain memory subsystem operations. Prefer MCP tools or a memory job model when MCP is available.
- CLI fallbacks are explicit recovery and diagnostic entry points; they must not become hidden authority or divergent memory semantics.

## Canonical Save Route
Session save must follow a fixed two-tier route. The mandatory tier always runs when save is invoked; the conditional tier writes additional records only when the session produced material change.

When the bundled MCP server is available, the `harness_session_save` tool writes the canonical working set, optionally appends the session log entry, and surfaces the post-save verification obligation. It does not execute project verification commands itself. When unavailable, the agent must walk the same route manually against the canonical Markdown layer.

`harness_session_save` is a material-boundary tool, not a heartbeat. Call it after major milestone completion, important decision changes, handoff points, or before stopping work. Do not call it after every small edit or routine verification, because save payloads and log entries consume context and create unnecessary continuity noise.

### Mandatory Tier
Every save must perform these steps in order:

0. **Reconcile the save draft against recent context.** Before writing, the agent must compare its draft save parameters against (a) the prior `memory/working-set.md` Open Questions and any deferred items, and (b) unresolved items raised in the current session's recent conversation that are still present in the agent's live context. Newly discovered gaps, deferred user concerns, and "we haven't done X" admissions must be carried forward into the save rather than silently dropped. This is a **context-local discipline rule** — it does not mandate rereading full session history from disk; it only requires the agent to verify that what is already in its working context since the previous save is reflected in the new save. Tools may assist by surfacing diffs (for example, open questions present in the prior working set but absent from the new draft), but the obligation is on the agent's judgment.
1. Update `memory/working-set.md` with the current goal, active plan, current decisions, open questions, next actions, restore checklist, save checklist, and authority warnings. Refresh the `updated_at` timestamp.
2. Append a dated entry to `memory/logs/YYYY-MM/YYYY-MM-DD.md` when the session produced durable decisions, document changes, verification results, or unresolved handoff items. A save with no new durable state must not write a filler entry.
3. Run the workspace's harness verification command after the writes. The save tool must surface this obligation with `verification_required` and `verify_hint`; the agent or caller records the actual result after running the verifier.
4. When the MCP server is available, keep changed memory records' derived DB/chunk/FTS/vector rows in sync through the normal write path. If a write reports derived-index drift, run the indicated `memory_runtime_repair` scope from canonical Markdown.

### Conditional Tier
Write additional records only when material change occurred:

5. Add or update `memory/known-facts/*.md` entries when the session produced new durable facts or superseded existing ones.
6. Update `memory/preferences.md` when the user expressed a stable, narrow preference that should survive across sessions.
7. Update `docs/plans/active/*` when the session changed plan content; move completed plans to `docs/plans/completed/`.
8. Invoke `memory_propose_promotion` when memory should escalate into law, plan, or reference rather than remain memory.

### Boundaries
- A save must never introduce governing content into memory. If the session produced new governing rules, update the law documents directly and let memory point to those files.
- Keep `memory/working-set.md` compact. Move history into logs instead of letting the working set become a changelog.
- Save responses should include a compact `promotion_reminder` so agents review material session output against the promotion protocol. This reminder may surface source ids produced by the save, such as `log_entry_id`, but must not select promotion candidates.

## Promotion Proposal Protocol
Promotion proposals are agent-triggered review requests. Runtime provides the tool, source ids, and reminder text; it does not decide which memories deserve promotion.

An agent must call `memory_propose_promotion` when the information has all three properties:

- durability: it should survive beyond the current session
- future operational relevance: future agents are likely to need it for operation, governance, or implementation
- authority gap: memory is too low-authority for the information, or a reviewed artifact should own it

Do not propose promotion for routine progress updates, one-off instructions, temporary debugging observations, unverified guesses, or facts already represented at the correct authority level.

Use existing source ids from `memory_save_item`, `memory_append_log`, `memory_get`, `memory_search`, `memory_list`, `memory_recent_logs`, or `harness_session_save.log_entry_id`. Do not invent source ids. A proposal remains an append-only memory log entry and must not directly edit law, plans, references, shared artifacts, or durable memory items.

## Memory Intent Rule
An agent must not promise or accept durable memory without resolving that memory intent.

Memory intent exists when the user or agent expresses an intent to remember, persist, carry forward, rely on information in a future session, or add information to future operating context. Examples include "remember this", "keep this for later", "from now on", "next session should know", "I will remember", "this is durable", and "this should go into the plan/law/reference".

Before final response, memory intent must resolve to one of:

- **`memory_write`** — call an appropriate memory write tool such as `memory_save_item`, `memory_append_log`, or `harness_session_save`. Use when the substance is not yet captured anywhere durable and the memory layer is the right home for it.
- **`promotion_proposal`** — call `memory_propose_promotion` when the information belongs in law, plan, reference, or shared artifact. Use when the substance has elevated authority (multi-session governance, project-wide constraint, structural rule).
- **`associative_marker`** — call `memory_save_item(type="fact", tags=["user-requested-memory", ...])` to write a short navigation breadcrumb pointing at the substance's existing location. Use when (a) the substance is already captured in another durable artifact (tracker entry, plan section, law clause, code path, or even a prior memory item / log entry) AND (b) the user expresses an intent to retrieve it later via a query like "what did I ask to remember", "we agreed earlier", or similar associative recall. The marker body is short (2-5 lines): the trigger phrase, the date, the topic, and a pointer to the substance. Vector + FTS indexing on the marker body lets future `memory_search("user requested memory")` or `memory_list(tags=["user-requested-memory"])` queries surface the marker, and the marker's pointer takes the agent to the real artifact. The marker does not duplicate the substance.
- **`explicit_non_save`** — explicitly state that the information is transient, already represented at the correct authority level, or otherwise not durable and will not be saved. Use when the substance is already adequately governed (e.g., the immediately surrounding code expresses it) **and** there is no associative-recall expectation. If the user might later ask "what did I ask to remember", `associative_marker` is the correct path instead.

The **discriminator between `memory_write` and `associative_marker`** is the same Q1 from the §Log Write Criterion: "Is this already captured in a durable artifact?" If the answer is yes, `memory_write` would create duplication; the marker writes only the breadcrumb. The discriminator between `explicit_non_save` and `associative_marker` is whether the user expresses associative-recall intent — if they do, the marker is required because `explicit_non_save` provides no future retrieval path.

When the substance pointer resolves to a memory id (a prior log entry, a known-fact, or another item) rather than a repository path, the marker body should record the id explicitly: e.g., `Substance: see memory_get(id="log/2026-04-26/some-decision")`. This keeps the marker discoverable through `memory_search` and the substance retrievable through the existing read tools.

This rule does not require saving every user message or every acknowledgement. It applies when the agent or user creates persistence expectations. Runtime reminders may surface this rule, but runtime must not select semantic memory or promotion candidates.

## Write Discipline
This section sets the criteria an agent should apply *before* invoking memory write tools, so that memory volume is governed by selectivity rather than by frequency or by token-cost avoidance. It complements the Canonical Save Route (mechanical procedure), the Promotion Proposal Protocol (escalation gate), and the Memory Intent Rule (resolution requirement). Four of the seven subsections below define stable `§` anchors that MCP tool descriptions reference directly: `§Log Write Criterion`, `§Item Write Criterion`, `§Read Routing Criterion`, and `§Log Body Format`. A fifth anchor referenced from these tool descriptions, `§Promotion Proposal Protocol`, is defined elsewhere in this file. Do not rename any of these five headings without updating the corresponding tool descriptions and reference docs in the same change.

### Log Write Criterion
A finding, decision, or correction should be written via `memory_append_log` when, and only when, all three questions resolve to "yes — log":

- **Q1.** Is this already captured in a durable artifact (plan, commit message, docstring, law, reference)? *Yes →* either skip the log, or write a pointer log only. *No →* go to Q2.
- **Q2.** Does this need to survive beyond the current turn's working context — across the next turn, a future session, or post-compaction recall? *No →* skip. *Yes →* go to Q3.
- **Q3.** Could a fresh agent, starting from the current source state, reproduce this finding with one targeted Grep or Read? *Yes →* skip; the source itself is sufficient. *No →* log.

A "no" on Q3 is signaled by any of: cross-file reasoning required, law plus code interpretation combined, an option-rejection rationale that the surrounding source does not explain, user-intent dependence, or surprise / correction relative to a prior expectation. Any single signal is sufficient to take the log path.

### Item Write Criterion
A durable item should be written via `memory_save_item` when, and only when, the content first passes the Log Write Criterion *and* additionally satisfies an applicability gate:

- The content must apply to multiple future situations. A `rule` should govern future agent behavior; a `fact` should persist as current state that future agents will read.

Content that passes the Log Write Criterion but not this applicability gate should remain a log entry. Items are vector-indexed and shape `memory_search` semantic results, so the cost of a poorly scoped item is paid every time recall runs against it.

### Read Routing Criterion
Before searching, classify the question to choose the right read surface. The agent should not default to either memory or source; routing follows the question's nature.

- **Q1.** Prior judgment, decision, or rationale ("why did we…", "what did we decide…", "근거")? → `memory_search` or `memory_recent_logs` first; source as follow-up if needed.
- **Q2.** Cross-session continuity ("어디까지 진행", "outstanding", "current state at session start")? → memory or working-set first; source follow-up.
- **Q3.** Current source / law fact ("what does this code do now", "what does the law say")? → `Grep` / `Read` first; memory not consulted unless the answer requires historical rationale.
- **Q4.** Mixed (current behavior plus historical rationale)? → both, in the order the response needs first.
- None of the above → refine the question before searching.

Signal vocabulary that triggers Q1 or Q2: "we", "previously", "earlier", "왜", "근거", "어디까지". Signal vocabulary that triggers Q3: "지금", "현재", "what does X do".

### Classify-Before-Search Rule
A short-form summary of the Read Routing Criterion, retained for working-set reinforcement: source / law facts → `Grep` / `Read`; prior judgment or cross-session context → `memory_search` (or `memory_recent_logs`); both → both; ambiguous → refine the question before searching.

### Log-Before-Answer Rule
When a turn's investigation produces a finding the answer will cite, the log entry should be written before the answer is composed, not after. The log is part of the answer, not a trailing artifact. This prevents the post-hoc batching failure mode where decisions visible at composition time are gone by the time the agent intends to log them.

### Log Body Format
Once a write passes the Log Write Criterion, body discipline applies:

- **Finding / Evidence / Resolution template.** The body should contain a Finding (one sentence — what was discovered, decided, or corrected), Evidence (file:line references or artifact paths), and Resolution (one sentence — outcome or pointer to the plan / source for full context).
- **Tag-first.** Classifiable metadata — topic categories, types, file references like `file:write.py`, status labels like `option-a-recommended` — should go into the `tags` parameter, not into body prose. Tags are indexed in the `memory_*_tags` SQL tables and are queryable via `memory_list(tags=…)`. Body prose with tag-shaped content duplicates tokens and lowers retrieval precision.
- **Plan summary, not plan copy-paste.** When a log references a mutable plan, spec, or working-set entry, the body should include a one-to-three-sentence summary of the relevant decision plus the plan path as a pointer. Pure pointers lose meaning if the plan is revised or archived; pure copy-paste creates drift and wastes tokens. Summary plus pointer is the rule.

Target shape: roughly 200–400 characters of body and 5–8 tags per log entry.

### Consult-Before-Answer Rule
For question shapes that depend on prior judgment, decision, rationale, or cross-session continuity, the agent **should consult memory before composing the answer**, not as a follow-up after the answer is delivered. This rule is the read-side counterpart of the §Log-Before-Answer Rule on the write side: both sides require that the memory layer is part of the response, not an afterthought.

The trigger signal is the same vocabulary that drives the §Read Routing Criterion's Q1 and Q2 paths: "we", "previously", "earlier", "왜", "근거", "어디까지", "outstanding", and the multilingual analogues. A question that includes any of these signals **and** is not already pinned to a known id by the user's exact phrasing should pass through `memory_search` (or `memory_recent_logs` for time-bounded recall) before the agent commits to an answer. A question that maps to Q3 of the Read Routing Criterion (current source / law fact, signaled by "지금", "현재", "what does X do") does **not** trigger this rule — `Grep` / `Read` is the right read surface there, and consulting memory first would create noise.

The discriminator from §Read Routing Criterion: that rule chooses the **right read surface** for any question; this rule pins the **timing** at which a memory-routed question must consult memory (before answering, not after). The two rules compose — Read Routing Criterion picks the surface, Consult-Before-Answer Rule pins when to use it.

This rule does not require consulting memory for every turn or for routine acknowledgements. It applies when the question's shape places the answer's correctness in the memory layer's path (prior decisions, prior judgments, cross-session continuity). Silence remains a valid answer when the memory query returns nothing relevant — failing to find prior judgment is itself a finding the agent can surface.

### Ambiguity Dissolver
The earlier "when in doubt, log" shortcut is superseded by criterion-first selection. The agent should apply the Log Write Criterion. If the content does not pass, the agent should write nothing — silence is a valid answer. If the content passes the log criterion but not the item applicability gate, log only. If both pass, write an item.

This framing makes non-writing a first-class outcome, preventing both the "skip everything to avoid cost" failure mode and the "log everything to feel productive" failure mode.

## Timestamp Integrity
Every timestamp written into a memory record must be the **actual wall-clock time at the moment of the write**, not a fabricated, sequenced, or approximated value. This applies to every time field in the canonical memory layer, including but not limited to:

- `updated_at` in `memory/working-set.md` YAML front matter
- `occurred_at` and `recorded_at` in `memory/logs/*` entries
- `created_at`, `updated_at`, and `last_verified_at` on known-fact and preference records
- `applied_at` in the pip package's `schema_version` table
- any timestamp embedded in an MCP tool response

Timestamps are load-bearing: the Canonical Restore Route uses the working set's `updated_at` to detect governance drift (law files modified after that time are flagged); `memory_recent_logs` uses `occurred_at` for time-window filtering; conflict resolution uses `recency` as the final tiebreaker after authority and scope. Fabricated timestamps silently break every one of these mechanisms.

Agents and tools that write memory records must obtain the current time from the operating system (e.g., `date` on a shell, `datetime.now(timezone.utc)` in Python) at write time. Sequencing several log entries with `+15 minutes` increments, copying a prior timestamp, or estimating a timestamp from context is a governance violation and should be corrected through `HARNESS_FIX_TOOL.md`.

When the `rule-harness` CLI is available, agents should prefer `rule-harness now --json` for manual timestamp lookup before writing memory records by hand. The command returns compact OS-clock UTC and local timestamps so agents do not spend prompt tokens composing ad hoc date commands or infer dates from conversation context. MCP tools and package code may continue to call the host language's clock directly when they write timestamps internally.

Pre-existing records with known-fabricated timestamps may be left in place when the order is still correct and the cost of rewriting exceeds the diagnostic value, but any new writes must use actual wall-clock time.

## Memory Tool Surface (MCP)
The LLM accesses indexed memory through the bundled MCP server only. Raw SQL against `.harness/index/meta.db` and direct file reads of derived state are prohibited.

The full tool surface is exposed as default capabilities. The signature reference (parameters, return shapes, error semantics) lives at `docs/references/harness-mcp-tools.md`. This document owns *when* and *why* to use each tool.

### Read Tools
- **`memory_search`** — combined FTS + vector search over memory item chunks. Use when the agent needs semantic or lexical recall across current memory; prefer it over reading full files. Logs remain historical records and are fetched through log-specific tools or known ids, not default semantic recall.
- **`memory_get`** — fetch a single item or log entry by id. Use when an id is already known (from a search hit, working set pointer, or supersession chain).
- **`memory_read_source`** — read a closed-set canonical memory source object from `memory/*` without using derived SQL/FTS/vector content for the returned text. Use it when exact source confirmation is needed for `working_set`, `lookup_rules`, `preferences`, `known_fact`, or `log_entry`. It is not a general Markdown, repository file, law, plan, reference, or source-code reader.
- **`memory_list`** — filtered list of items by type/scope/status/tag. Use for "show me all active facts about X" style queries. Accepts `type="rule"` to list local governance rules, with optional `applies_when` filter for trigger-tag matching.
- **`memory_recent_logs`** — recent log entries within a time window. Use for "what happened in the last N days" rather than scanning files.

### Write Tools
- **`memory_save_item`** — create or update a `fact`, `preference`, or `rule`. Writes canonical Markdown first, then updates derived DB/chunk/tag/vector rows. If the derived update fails after the Markdown write, the tool must report `memory.derived_index_drift` and point to `memory_runtime_repair`. Rules follow the rule front matter schema defined in "Local Rule Memory Type". Preferences follow the section-aware grammar defined under "Preferences File / Machine-Writable Grammar". Calls with `type="working_set_entry"` return `memory.invalid_params`; the working-set file is rewritten in full by `harness_session_save` rather than addressed at item-level.
- **`memory_append_log`** — append a log entry to today's log file. Append-only; cannot edit prior entries. Writes canonical Markdown first, then updates derived DB/chunk/tag rows. V1 does not create log vector rows.

### Lifecycle Tools
- **`memory_set_status`** — transition an item's status (active → stale, etc.) following the conflict rules in this document.
- **`memory_supersede`** — mark an item as `superseded` and link it to a replacement item in one operation.

### Governance Tool
- **`memory_propose_promotion`** — create a candidate for reviewed promotion of memory into law, plan, reference, or shared artifact. **Must not** directly promote; only proposes for review through the normal change path.

### Session Tools
- **`harness_session_restore`** — implements the Canonical Restore Route above; returns the compact packet and runtime `authority_recall` guidance, including the Memory Intent Rule.
- **`harness_session_save`** — implements the Canonical Save Route above and returns `promotion_reminder` plus `memory_intent_reminder` reminders without runtime-selected candidates.

### Operational Tools
- **`memory_runtime_check`** — preview a memory runtime repair from canonical Markdown without creating a job row or mutating DB, FTS, or vector tables. Use before repair when source drift is suspected and the agent needs counts/warnings only.
- **`memory_runtime_repair`** — repair derived DB/chunk/FTS state and item vector rows from canonical Markdown. Use when source drift is detected, after a schema migration, or after an embedding model change. Log vector rows are not generated in v1; log/all repair removes accidental log vector rows while preserving log records and log chunks. Runtime repair is globally single-flight; if another repair is queued/running, poll that active job instead of starting a second one.
- **`memory_runtime_repair_start`** — start a pollable runtime repair job for long-running work that may exceed agent-host MCP tool-call timeouts. It follows the same global single-flight guard as synchronous repair.
- **`memory_runtime_repair_status`** — return status and result for a runtime repair job. Queued/running jobs that exceed the stale threshold are surfaced as failed with `memory.job_stale` rather than left active forever.
- **`memory_health_check`** — report runtime status (MCP server, exact index, vector index, embedding model availability).

### Selection Rules
- If you need to find memory you don't already know about → `memory_search`.
- If you have an id and need the indexed record shape → `memory_get`.
- If you need exact canonical memory source text for an allowed memory source object → `memory_read_source`.
- If you need a list filtered by metadata → `memory_list`.
- If you need recent activity → `memory_recent_logs`.
- If you would otherwise read a memory file by hand → use the appropriate tool instead. Direct file reads are allowed only as the documented degradation path when the MCP server is unavailable.

## Evaluation Boundary
Evaluation methodology, benchmark fixtures, recall metrics, and token-efficiency experiments belong to recursive improvement and quality verification, not to the default shared distribution contract.

Shared distribution documents should contain operational memory rules, authority boundaries, artifact layout, and minimum verification expectations. They should not require every target repository to carry Harness Memory benchmark suites.

## Next Update Trigger
Update this document when:
- memory layout changes
- conflict statuses change
- session restore behavior changes
- protocol capability names change
- repeated memory failures show that stronger operational rules are needed
