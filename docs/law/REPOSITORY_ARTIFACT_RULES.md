# Repository Artifact Rules

## Purpose
This document defines how repository artifacts are named, where they belong, and when new artifact structures may be introduced.

Its goal is to prevent structural drift as the project grows.

## Artifact Classes
The default repository artifact classes are:
- constitutional documents
- law documents
- contract documents
- execution-entry documents
- root control documents
- plans
- references
- trackers
- generated or derived artifacts when they become necessary
- runtime continuity or memory data when governed
- install manifests or package-template artifacts when installable distribution is explicitly approved

Every new artifact should be classified into an existing class before a new class or directory is proposed.

## Execution-Entry Document Definition
Execution-entry documents are short routing documents that tell an agent how to enter the work correctly.

Their role is to:
- point to the actual source-of-truth documents
- define read-first order
- state current work order or entry priorities
- give short operational guardrails for entering the work

They may include:
- repository identity and purpose at a high level
- source-of-truth order
- read-first order
- current priority
- short working rules and anti-patterns

They must not include:
- project-specific scope details that belong in law documents
- detailed input/output contracts
- detailed oracle or acceptance logic
- detailed failure rules
- long-form plans, references, or design notes

The default execution-entry document is `AGENTS.md`.

## Execution-Entry Pre-Write Gate
Before adding content to an execution-entry document, the agent must classify the content:

- **Routing content** (allowed): read-first order, source-of-truth pointers, current priority, short entry guardrails
- **Governing content** (not allowed): scope boundaries, contract details, oracle logic, failure rules, artifact structure rules, process descriptions, boundary definitions

If the content is governing, place it in the owning law document instead.

This gate applies to every write to an execution-entry document, not only to initial creation. Gradual accumulation of governing content through incremental additions is the primary failure mode this gate prevents.

When in doubt, check whether the content would still make sense as a standalone rule in a law document. If yes, it belongs there rather than in the execution-entry document.

## Operational Artifact Definition
Operational artifacts are structured supporting records used to help agents work safely as a project grows.

Their role is to:
- track meaningful work over time
- preserve searchable repository-local references
- record debt, drift risks, and promotion candidates
- store stable derived facts when repeated work depends on them

Operational artifacts may include:
- active plans
- completed plans
- reference notes
- tracker entries
- code-authorship coverage matrices or refactor-hardening notes when needed to make verification coverage visible
- generated or derived repository facts that satisfy the repository rules

Examples of stable derived facts include:
- a recorded shared-harness baseline used for later incremental update comparison

Operational artifacts must not:
- replace constitutional or law-layer authority
- silently redefine scope, contracts, oracle logic, or failure handling
- become an unstructured dump of chat history or temporary notes

The default operational-artifact paths are `docs/plans/*`, `docs/references/*`,
`docs/planning-protocol/*`, and stable generated-facts locations when explicitly
approved by repository rules.

An operational artifact counts as approved only when:
- it fits an allowed artifact class and path
- it passes the required pre-creation checks when a new structure or artifact type is involved
- required governing-document synchronization has been completed or explicitly deferred with visible impact

## Runtime Continuity And Memory Data
Runtime continuity or memory data is a default lower-authority derived artifact class.

It may be used only when:

- the runtime path is documented before use
- authority is labeled as derived continuity
- conflicts with repository files are resolved by `authority > scope > recency`
- conflicting memory changes status rather than silently overwriting source material
- reset, export, audit, and repair expectations are documented at the current maturity level

Default split-source layout:

```text
memory/
  known-facts/
  logs/
  rules/
  preferences.md
  working-set.md
  LOOKUP_RULES.md
.harness/
  index/
    meta.db                # SQLite + FTS5 + sqlite-vec virtual tables
  models/
    embedding/
      <model-name>/        # downloaded by agentlaw init
  cache/
  jobs/
```

In this model, `memory/*` is the human-reviewable canonical memory layer and `.harness/*` is derived runtime/index state. Vector embeddings are co-located with metadata in `meta.db` via the `sqlite-vec` extension, so there is no separate vector directory.

`.harness/` is a governed default runtime path for Harness memory indexes, embedding model artifacts, jobs, and caches. Its contents are derived state and may be rebuilt or degraded to canonical memory files.

`memory/` is a governed default canonical memory path. It stores lower-authority memory records, not law.

`memory/rules/` is the default path for project-local governance rules (the `rule` memory type). Rules sit below shared `docs/law/*` in authority and above known facts and preferences. Full framework — front matter schema, authority position, discoverability obligations, and distribution-boundary rule for `publish-repo/memory/rules/` — lives in `docs/law/MEMORY_AND_CONTINUITY_RULES.md` under "Local Rule Memory Type".

Memory source files, databases, indexes, logs, and context packets must not:

- create law-layer meaning
- replace `docs/plans/*`, `docs/references/*`, or generated facts when durable reviewable records are needed
- silently store the only copy of important governance decisions
- become a reason to enlarge `AGENTS.md`

Important memory-derived facts that need review should be exported or proposed for promotion into an approved repository artifact through the normal change path.

Earlier or narrower prototypes may use a reduced layout such as:

```text
.harness/
  memory/
    agentlaw-memory.sqlite
```

Such a reduced layout must be documented as a prototype-specific implementation choice, not as the final general memory architecture.

## Session Continuity Artifacts
Repository work that spans multiple sessions requires a session continuity path so that a new session can recover the current work state quickly and reliably.

Harness Memory restore behavior is governed by `docs/law/MEMORY_AND_CONTINUITY_RULES.md`.

The canonical session continuity artifacts live under `memory/*`.

### Required Memory Content
The memory continuity layer must record:
- **Current working set** in `memory/working-set.md`: current goal, active plan, current decisions, open questions, next actions, restore checklist, save checklist, and authority warnings
- **Lookup policy** in `memory/LOOKUP_RULES.md`: read order, trigger map, budgets, exclusions, and escalation rules
- **Durable handoff log** under `memory/logs/*`: dated session decisions, verification results, durable changes, and unresolved handoff items

### Update Obligation
The memory continuity layer must be updated:
- at the end of each meaningful session before context is lost
- whenever a session produces outcomes that change what the next session should do
- whenever pending items are completed or new ones arise

A session that changes repository state without updating memory continuity creates continuity drift.

### Verification Obligation
A new session must treat memory continuity as **stale until verified** against current repository state. This means:
- run the restore checklist before acting on recorded assumptions
- when memory disagrees with current repository state, trust the repository state and update memory
- do not treat memory as authoritative; it is a recovery aid below law and approved artifacts

### Content Boundary
Session continuity memory is memory, not law. It must not:
- create governing meaning
- restate rules that live in the constitution or law layer
- become a substitute for reading the actual governing documents

It may point to governing documents but must not duplicate their content.

### Failure Modes
Treat the following as session continuity failures:
- required memory continuity files are missing when work spans multiple sessions
- `memory/working-set.md` records stale assumptions that no longer match repository state
- memory grows to contain governing content that should live in law documents
- a session completes meaningful work without updating memory continuity

### Restore Request Protocol
When an agent is asked to restore session context via a short trigger such as `restore session`, `resume`, or an equivalent phrase, the agent should prefer the Harness Memory session restore protocol when available.

The agent must follow a minimal-read, minimal-output protocol:

- read `memory/working-set.md` first
- read `memory/LOOKUP_RULES.md` only when lookup policy or escalation decisions are needed
- read `memory/logs/*` only when the working set is insufficient
- respond with a terse summary targeting eight or fewer bullet lines, covering current goal, current decisions, open questions, and next actions
- do not preemptively read `docs/plans/*`, other `docs/law/*` files, other `docs/references/*` files, or completed plan history unless the user explicitly asks for deeper context
- still honor the Verification Obligation: present recorded state as stale until verified rather than asserting it as current fact
- expand scope only on explicit follow-up from the user

This protocol exists to keep session restoration cheap and repeatable across agents and across token-constrained resumption scenarios, including handoff between different agent runtimes. It is a behavior contract on the consumer side of memory continuity, complementary to the Update and Verification Obligations on the producer side.

### Save Request Protocol
When an agent is asked to save session context via a short trigger such as `save session`, `session save`, or an equivalent phrase, the agent should use the Harness Memory save protocol in `docs/law/MEMORY_AND_CONTINUITY_RULES.md`.

The agent must update memory continuity according to the following protocol:

- **Update current state**: refresh `memory/working-set.md` with the current goal, active plan, current decisions, open questions, next actions, restore checklist, save checklist, and authority warnings.
- **Append durable history**: append a dated entry under `memory/logs/*` covering meaningful outcomes, verification results, durable decisions, and unresolved handoff items.
- **Remove stale state**: remove or rewrite working-set entries that no longer inform next-session behavior. The working set is a recovery aid, not a changelog; logs and git history carry history.
- **Verify outgoing state**: the recorded state must reflect actual repository reality at save time, not conversational claims. When the session made file changes, confirm the edits landed. When the session made commits, reference them concretely.
- **Respect content boundary**: the save operation must never introduce governing content into memory. If a session produced new governing rules, those rules belong in law documents; only a short pointer to the rule change belongs in memory.
- **Keep memory lean**: keep `memory/working-set.md` compact. Move history into logs instead of letting the working set accumulate.

A save request without any new meaningful state to record is a no-op — do not write empty or filler entries.

This protocol operationalizes the Update Obligation into an explicit user-invoked trigger, so that multi-session work can be handed off deliberately rather than depending on the agent remembering to update at session end.

## Naming Rules
### Law Documents
Law documents use uppercase snake case.

Examples:
- `SCOPE.md`
- `INPUT_OUTPUT_CONTRACT.md`
- `ORACLE_AND_JUDGMENT.md`
- `FAILURE_TAXONOMY.md`
- `REPOSITORY_ARTIFACT_RULES.md`
- `STARTER_SPECIALIZATION_RULES.md`
- `MECHANICAL_ENFORCEMENT_POLICY.md`

### Execution-Entry Documents
Execution-entry documents use stable conventional names.

Examples:
- `AGENTS.md`

### Root Control Documents
Root control documents use stable conventional names and live at repository root.

Examples:
- `AGENTLAW_INIT_TOOL.md`
- `AGENTLAW_UPDATE_TOOL.md`
- `AGENTLAW_FIX_TOOL.md`

Harness bootstraps from upstream are performed via `AGENTLAW_INIT_TOOL.md`; that document carries its own Full Bootstrap Cycle, Responsibility Split, and Non-Pip Distribution sections. Harness updates from upstream are performed via `AGENTLAW_UPDATE_TOOL.md`; that document carries its own Full Update Cycle, Responsibility Split, Failure Modes, and Non-Pip Distribution sections. The pip package's `pipx upgrade agentlaw` brings the new shared kit and applies binary schema migrations; the LLM-driven `AGENTLAW_UPDATE_TOOL.md` procedure incorporates governance content into local law and artifacts. Skipping the LLM-driven procedure after a `pipx upgrade` is a governance violation: it leaves binary infrastructure ahead of local law.

### Plan Documents
Plan documents use lowercase kebab case with names that describe the work clearly.

Examples:
- `todo-first-release-plan.md`
- `local-persistence-rollout.md`
- `task-filtering-followup.md`

### Active Plan Preflight Fields
For active plans that govern non-trivial or high-risk implementation work, the plan must include these fields or explicitly mark them as not applicable:

- scope
- affected surfaces
- public contract impact
- test or mechanical oracle
- documentation and reference impact
- install, packaging, upgrade, or existing-target impact
- cross-platform or environment impact
- rollback, repair, or recovery path
- explicit non-goals
- new reference or law files proposed, if any, each with a justification for why the content does not fit in an existing artifact under its defined role

Plans for trivial cleanup may stay smaller, but they must not hide public-contract, runtime, package, configuration, schema, or cross-component impact.

The `Affected surfaces` bullet list must contain repo-relative paths or globs as inline-code (backticks). Free-form prose may surround each path, but each bullet must include at least one path or glob in backticks. The verifier extracts the backticked tokens and matches them against changed-file paths via Python's `pathlib.PurePath.match`, with directory-prefix shortcut: a token ending in `/` matches any descendant. This is the parseable contract that a mechanical plan-coverage check relies on; bullets that name no path in backticks contribute nothing to coverage.

### Contract Documents
Contract documents use lowercase kebab case. They describe a boundary surface shared with target projects and must carry an authority header declaring their domain, verification check, and governing law anchor.
They must reside in `docs/contracts/` and be mirrored to `publish-repo/docs/contracts/`.

Examples:
- `shared-agentlaw-baseline.md`
- `agentlaw-update-workflow.md`
- `agentlaw-mcp-tools.md`

### Reference Documents
Reference documents use lowercase kebab case and should signal their purpose when possible. They are local to the authoring repository by default.

Examples:
- `local-storage-reference.md`
- `sqlite-notes.md`
- `design-system-reference.md`

### Tracker Documents
Trackers should prefer stable fixed names over ad hoc variants.

Examples:
- `tech-debt-tracker.md`

### Planning Protocol Documents
Planning protocol documents use lowercase kebab case and live under
`docs/planning-protocol/`.

They provide operational guidance for request classification, plan review
method, and persona deck selection. They are shared with downstream projects,
but they remain below the law layer.

Law defines the following finite set of shared planning protocol files
distributed in `publish-repo/docs/planning-protocol/`: `README.md`,
`task-classification.md`, `review-method.md`, `persona-decks-core.md`,
`persona-decks-specialized.md`. Adding, renaming, or removing a file requires a
plan that edits this enumeration in the same change; `agentlaw verify`
mechanically asserts that `publish-repo/docs/planning-protocol/` contains this
exact set and no unratified sibling files.

## Naming Anti-Patterns
Do not use:
- ambiguous names such as `misc.md`, `stuff.md`, or `notes2.md`
- unstable suffixes such as `final`, `final2`, `new`, or `temp`
- names that hide the artifact class or purpose
- multiple naming styles for the same artifact type

## Default Directory Rules
The default repository directories are:
- `docs/law/` for governing law documents
- `docs/contracts/` for contract documents shared with target projects
- `docs/plans/active/` for active versioned plans
- `docs/plans/completed/` for completed plans kept for history
- `docs/references/` for searchable repository-local references
- `docs/planning-protocol/` for shared operational planning classification and review protocol

These default directories should be used before introducing new top-level or peer directories.

`memory/` and `.harness/` are part of the default Harness memory structure. They remain governed lower-authority paths and must not become hidden law.

The default root-level constitution, entry, and control documents are:
- `AGENTLAW_CONSTITUTION.md`
- `AGENTS.md`
- `AGENTLAW_INIT_TOOL.md`
- `AGENTLAW_UPDATE_TOOL.md`
- `AGENTLAW_FIX_TOOL.md`

## Directory Creation Gate
Creating a new directory is not the default response to project growth.

A new directory may be introduced only when:
- the current artifact classes cannot represent the work without semantic confusion
- the need is repeated rather than one-off
- the new directory has a stable purpose that can be explained in repository rules
- the change is recorded in the governing documents before it becomes normal practice

Convenience alone is not enough.

## Required Pre-Creation Checks
Before creating a new directory, artifact type, or new section / rule / sub-section inside an existing governed artifact, the agent must:
1. classify the change using the repository change classification rule
2. verify that existing law documents, plans, references, or §s within governed artifacts cannot already hold the content safely
3. record the need in `docs/plans/tech-debt-tracker.md` when the structure change is likely to recur
4. identify which governing documents must be synchronized if the structure is added

### New Reference File Rule
Before creating a new file under `docs/references/`, the agent must identify any existing reference whose role overlaps with the proposed content.

- If no overlapping reference exists, proceed.
- If an overlapping reference exists, the agent must either place the content in the existing file, or justify in the governing plan why a separate file is needed.
- The justification must be visible in the plan, not only in conversation or commit messages.

### New Section / Rule Addition Rule
Before adding a new section (`##` or `###`), rule, or sub-section to a law document under `docs/law/*`, a root control document (`AGENTLAW_CONSTITUTION.md`, `AGENTLAW_INIT_TOOL.md`, `AGENTLAW_UPDATE_TOOL.md`, `AGENTLAW_FIX_TOOL.md`), a contract document under `docs/contracts/*`, or a reference document under `docs/references/*`, the agent must first search the target artifact (and its sibling artifacts in the same class) for any existing rule whose **intent** overlaps with the proposed content.

The search obligation is satisfied only when the agent has used **multiple synonym variants of the intended rule title and intent**, not a single keyword. A search that uses only the title the agent has in mind will systematically miss rules whose authors used different phrasing for the same intent. Concrete example: a proposed "Comment Self-Narration Prohibition" must also search the artifact for `implementation history`, `changelog`, `narration`, `rename history`, `historical context`, and the specific anti-patterns the proposed rule would prohibit. The obligation extends across the entire governed artifact set in scope, not only the file the new rule would land in.

When the search surfaces an existing rule whose intent overlaps:

- **Strengthen the existing rule.** The default action is to extend or sharpen the existing rule's text — add explicit anti-pattern examples, raise the prohibition's specificity, or add a cross-reference to the matching law on a sibling artifact.
- **Do not add a sibling §.** A new sibling § with the same intent fragments the rule across two locations and creates self-contradicting drift as the two § s evolve independently.
- **If a sibling § is genuinely required** (the existing rule's scope is too narrow and the proposed rule would meaningfully extend rather than duplicate the intent), the justification must be visible in the governing plan. The plan must name the existing rule it considered, summarize why strengthening was rejected, and explain the orthogonality of the new rule's scope.

This rule does not apply to factual material additions inside an existing § (adding a missing parameter to a contract document's parameter list, adding a new entry to a finite enumeration, fixing a typo). It applies to **rule** additions — text that imposes a new obligation, prohibition, or behavioral expectation.

Law defines the following finite set of shared reference roles distributed in `publish-repo/docs/references/`: `README`, `project-overview`. Adding, renaming, or removing a role requires a plan that edits this enumeration in the same change; `agentlaw verify` mechanically asserts that `publish-repo/docs/references/` contains no files outside this set (plus the local-only references under `docs/references/` in the authoring repo, which are unbounded by design and not part of this enumeration). Splitting a single named role across sibling files without law backing is governance drift. When authoring-workspace content and distribution-template content share the same role, they remain one file by role and must follow the mirror rules in `SCOPE.md` and the publish-seed boundary, not a new sibling file.

The `project-overview` role additionally carries a "Code architecture map" subsection. Its obligations — slot menu, minimum structure-plus-flow coverage, slot-selection rationale, `Map scope:` declaration, density cap, Mermaid-only format, slot-name headings, and the mtime-based Layer 2 staleness check — are specified in `docs/law/CODE_AUTHORSHIP_AND_STEWARDSHIP_RULES.md` under "Code Architecture Map".

## Expansion Decision Rule
Before creating a new directory or artifact class, answer these questions:
1. Can the content live in the existing law documents?
2. Can the content live in `docs/plans/active/*` or `docs/plans/completed/*`?
3. Can the content live in `docs/references/*`?
4. Can the problem be solved by a better file name instead of a new directory?
5. Is this need likely to recur across multiple meaningful changes?

If the answer to one of the first four questions is yes, do not create a new directory yet.

## Promotion Rule
When a new artifact type or structure seems necessary:
1. Record the need in `docs/plans/tech-debt-tracker.md`.
2. Clarify why existing directories are insufficient.
3. Update the law layer if the new structure affects project operation or agent routing.
4. Only then introduce the new directory or artifact type.

## Specialization Reference
Starter-law carry-forward and project-instance specialization rules live in:
- `docs/law/STARTER_SPECIALIZATION_RULES.md`

Use that document when the question is not artifact structure itself, but how starter protections must survive project-specific rewriting.

## Mechanical Enforcement Reference
Executable enforcement requirements for mechanically detectable constraints live in:
- `docs/law/MECHANICAL_ENFORCEMENT_POLICY.md`

Use that document when the question is not artifact structure itself, but whether prose must become tests, lint, CI, or structural checks.

## Shared Artifact Set Separation Rule
If the repository has multiple shared artifact sets or derived outputs, they must not silently collapse into one unmanaged operational file set.

When a change affects shared artifacts or derived outputs, confirm that:
- each shared artifact set still points to the intended generalized document set
- derived outputs are built from the intended shared source set
- no parallel shared artifact set silently keeps obsolete or conflicting meaning

Installable package templates, if introduced, count as a shared artifact set or derived output and must have an explicit source-of-truth relationship to the shared starter documents.

## Mandatory Synchronization Check
When artifact structure changes, the agent must explicitly review whether updates are needed in:
- `AGENTS.md`
- `AGENTLAW_CONSTITUTION.md`
- `docs/law/*`
- `docs/plans/tech-debt-tracker.md`
- other shared artifact sets or derived outputs when they exist

For memory or installable-distribution changes, also review:
- whether `.harness/` or manifest/config paths need artifact-rule coverage
- whether package templates or installer assets need synchronization rules
- whether memory exports should become approved generated facts or references

If any related artifact is intentionally not updated, that decision must be visible and justified.

When a governing document is split, renamed, moved, or replaced, update the affected read paths, ownership references, and synchronization points before treating the change as complete.

When governance changes affect shared starter behavior, package templates, generated artifacts, or public setup instructions, the agent must check the project's documented synchronization points before completion.

## Generated Or Derived Artifacts
Generated or derived repository artifacts should be added only when they are:
- important to repeated agent work
- versionable in a stable form
- difficult to recover reliably from external context alone

Until those conditions hold, do not introduce a dedicated generated-artifacts structure.

When the repository must later compare against an earlier shared harness version, a stable baseline record may live in:
- `docs/references/shared-agentlaw-baseline.md`

## Self-Narration Prohibition
Governed artifact bodies — law documents under `docs/law/*`, root control documents, contract documents under `docs/contracts/*`, and reference documents under `docs/references/*` — must describe the **current state** only. They must not narrate their own revision history inside the body. Phrases like "previously declared but never implemented", "renamed from X on YYYY-MM-DD", "added on YYYY-MM-DD", "the earlier formulation was Y", "as of YYYY-MM-DD this section was strengthened", or any equivalent in-body changelog narration are prohibited.

The reason: artifact bodies are read by every new agent on every session restore. Self-narration inflates the read budget on signal that is operationally irrelevant for the current state, biases the agent toward the historical formulation, and pollutes search retrieval with timestamps that should belong to a different layer.

History lives in the layers built for it. Plan documents under `docs/plans/active/*` and `docs/plans/completed/*` are themselves narratives of work and may carry their own Revision History sections. Tracker entries in `docs/plans/tech-debt-tracker.md` carry per-entry status changes. Memory log entries under `memory/logs/*` record dated decisions. Git commit history captures every artifact diff. The reader who genuinely needs "why was this rule changed?" follows pointers into those layers; they do not read the rule's own body for the answer.

This rule does not prohibit example anti-patterns shown inside fenced quotes (a body may demonstrate "do not write entries shaped like this" with a quoted example), reference-style cross-pointers ("see `docs/plans/completed/...` for the change history"), or stable terminology that incidentally contains a year (a feature name like `2024-rollover-spec` is naming, not narration). The rule prohibits in-body sentences that describe the artifact's own revision arc as part of the artifact's content.

The same prohibition extends to source-code comments and docstrings under `src/*` and to test comments under `tests/*` — see `docs/law/CODE_AUTHORSHIP_AND_STEWARDSHIP_RULES.md` §"Reasoning-Critical Inline Comments" (the self-narration bullet) for the parallel ruling on the code side.

## Maintenance Rule
If artifact naming becomes inconsistent or directories multiply without clear class boundaries, treat that as governance drift and repair it before adding more structure.

Repeated failures to classify, synchronize, or separate artifact structures should themselves be recorded as promotion candidates for stronger enforcement.
