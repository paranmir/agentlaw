# Repository Artifact Rules

## Purpose
This document defines how repository artifacts are named, where they belong, and when new artifact structures may be introduced.

Its goal is to prevent structural drift as the project grows.

## Artifact Classes
The default repository artifact classes are:
- constitutional documents
- law documents
- execution-entry documents
- root control documents
- plans
- references
- trackers
- generated or derived artifacts when they become necessary

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
- generated or derived repository facts that satisfy the repository rules

Examples of stable derived facts include:
- a recorded shared-harness baseline used for later incremental update comparison

Operational artifacts must not:
- replace constitutional or law-layer authority
- silently redefine scope, contracts, oracle logic, or failure handling
- become an unstructured dump of chat history or temporary notes

The default operational-artifact paths are `plans/*`, `references/*`, and stable generated-facts locations when explicitly approved by repository rules.

An operational artifact counts as approved only when:
- it fits an allowed artifact class and path
- it passes the required pre-creation checks when a new structure or artifact type is involved
- required governing-document synchronization has been completed or explicitly deferred with visible impact

## Session Continuity Artifact
Repository work that spans multiple sessions requires a session continuity artifact so that a new session can recover the current work state quickly and reliably.

The default session continuity artifact lives at `references/session-reminder.md`.

### Required Content
The session continuity artifact must record:
- **Recent work snapshot**: what meaningful work was completed in recent sessions, dated
- **Pending next session**: concrete open items the next session should pick up
- **Read order for new session**: the files a new session should read first, in order
- **First checks for new session**: concrete verification steps the new session must perform before acting

### Update Obligation
The session continuity artifact must be updated:
- at the end of each meaningful session before context is lost
- whenever a session produces outcomes that change what the next session should do
- whenever pending items are completed or new ones arise

A session that changes repository state without updating this artifact creates continuity drift.

### Verification Obligation
A new session must treat the artifact as **stale until verified** against current repository state. This means:
- run the listed first checks before acting on recorded assumptions
- when the artifact disagrees with current repository state, trust the repository state and update the artifact
- do not treat the artifact as authoritative; it is a recovery aid, not a source of truth

### Content Boundary
The session continuity artifact is a reference, not a law document. It must not:
- create governing meaning
- restate rules that live in the constitution or law layer
- become a substitute for reading the actual governing documents

It may point to governing documents but must not duplicate their content.

### Failure Modes
Treat the following as session continuity failures:
- the artifact is missing when work spans multiple sessions
- the artifact records stale assumptions that no longer match repository state
- the artifact grows to contain governing content that should live in law documents
- a session completes meaningful work without updating the artifact

### Restore Request Protocol
When an agent is asked to restore session context via a short trigger such as `restore session`, `resume`, or an equivalent phrase, the agent must follow a minimal-read, minimal-output protocol:

- read only the session continuity artifact (`references/session-reminder.md`) unless that artifact itself directs further reads
- respond with a terse summary targeting eight or fewer bullet lines, covering recent work snapshot and pending next-session items
- do not preemptively read `plans/*`, other `docs/harness/*` files, other `references/*` files, or completed plan history unless the user explicitly asks for deeper context
- still honor the Verification Obligation: present recorded state as stale until verified rather than asserting it as current fact
- expand scope only on explicit follow-up from the user

This protocol exists to keep session restoration cheap and repeatable across agents and across token-constrained resumption scenarios, including handoff between different agent runtimes. It is a behavior contract on the consumer side of the session continuity artifact, complementary to the Update and Verification Obligations on the producer side.

### Save Request Protocol
When an agent is asked to save session context via a short trigger such as `save session`, `session save`, or an equivalent phrase, the agent must update `references/session-reminder.md` according to the following protocol:

- **Inject new state**: prepend a new dated entry to the Recent Work Snapshot covering meaningful work completed in the current session. Each entry must describe outcomes, not narration, and must include the motivation when the motivation is not obvious from the outcome.
- **Remove stale state**: remove or collapse snapshot entries whose outcomes are now simply baseline repository state and no longer inform next-session behavior. The artifact is a recovery aid, not a changelog — git history is the authoritative changelog. Do not preserve full session history inside this file.
- **Refresh pending list**: remove pending items completed during this session, rewrite items whose shape changed, and add new pending items discovered this session.
- **Verify outgoing state**: the recorded state must reflect actual repository reality at save time, not conversational claims. When the session made file changes, confirm the edits landed. When the session made commits, reference them concretely.
- **Respect content boundary**: the save operation must never introduce governing content into the artifact. If a session produced new governing rules, those rules belong in law documents; only a short pointer to the rule change belongs in the snapshot.
- **Keep the artifact lean**: if the file grows past the point where a new session can absorb it quickly, aggressively collapse older sections rather than letting it accumulate.

A save request without any new meaningful state to record is a no-op — do not write empty or filler entries.

This protocol operationalizes the Update Obligation into an explicit user-invoked trigger, so that multi-session work can be handed off deliberately rather than depending on the agent remembering to update at session end.

## Naming Rules
### Law Documents
Law documents use uppercase snake case.

Examples:
- `HARNESS_SCOPE.md`
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
- `HARNESS_INIT_TOOL.md`
- `HARNESS_UPDATE_TOOL.md`
- `HARNESS_FIX_TOOL.md`

### Plan Documents
Plan documents use lowercase kebab case with names that describe the work clearly.

Examples:
- `todo-first-release-plan.md`
- `local-persistence-rollout.md`
- `task-filtering-followup.md`

### Reference Documents
Reference documents use lowercase kebab case and should signal their purpose when possible.

Examples:
- `local-storage-reference.md`
- `sqlite-notes.md`
- `design-system-reference.md`

### Tracker Documents
Trackers should prefer stable fixed names over ad hoc variants.

Examples:
- `tech-debt-tracker.md`

## Naming Anti-Patterns
Do not use:
- ambiguous names such as `misc.md`, `stuff.md`, or `notes2.md`
- unstable suffixes such as `final`, `final2`, `new`, or `temp`
- names that hide the artifact class or purpose
- multiple naming styles for the same artifact type

## Default Directory Rules
The default repository directories are:
- `docs/harness/` for governing law documents
- `plans/active/` for active versioned plans
- `plans/completed/` for completed plans kept for history
- `references/` for searchable repository-local references

These default directories should be used before introducing new top-level or peer directories.

The default root-level constitution, entry, and control documents are:
- `HARNESS_CONSTITUTION.md`
- `AGENTS.md`
- `HARNESS_INIT_TOOL.md`
- `HARNESS_UPDATE_TOOL.md`
- `HARNESS_FIX_TOOL.md`

## Directory Creation Gate
Creating a new directory is not the default response to project growth.

A new directory may be introduced only when:
- the current artifact classes cannot represent the work without semantic confusion
- the need is repeated rather than one-off
- the new directory has a stable purpose that can be explained in repository rules
- the change is recorded in the governing documents before it becomes normal practice

Convenience alone is not enough.

## Required Pre-Creation Checks
Before creating a new directory or artifact type, the agent must:
1. classify the change using the repository change classification rule
2. verify that existing law documents, plans, or references cannot already hold the content safely
3. record the need in `plans/tech-debt-tracker.md` when the structure change is likely to recur
4. identify which governing documents must be synchronized if the structure is added

## Expansion Decision Rule
Before creating a new directory or artifact class, answer these questions:
1. Can the content live in the existing law documents?
2. Can the content live in `plans/active/*` or `plans/completed/*`?
3. Can the content live in `references/*`?
4. Can the problem be solved by a better file name instead of a new directory?
5. Is this need likely to recur across multiple meaningful changes?

If the answer to one of the first four questions is yes, do not create a new directory yet.

## Promotion Rule
When a new artifact type or structure seems necessary:
1. Record the need in `plans/tech-debt-tracker.md`.
2. Clarify why existing directories are insufficient.
3. Update the law layer if the new structure affects project operation or agent routing.
4. Only then introduce the new directory or artifact type.

## Specialization Reference
Starter-law carry-forward and project-instance specialization rules live in:
- `docs/harness/STARTER_SPECIALIZATION_RULES.md`

Use that document when the question is not artifact structure itself, but how starter protections must survive project-specific rewriting.

## Mechanical Enforcement Reference
Executable enforcement requirements for mechanically detectable constraints live in:
- `docs/harness/MECHANICAL_ENFORCEMENT_POLICY.md`

Use that document when the question is not artifact structure itself, but whether prose must become tests, lint, CI, or structural checks.

## Shared Artifact Set Separation Rule
If the repository has multiple shared artifact sets or derived outputs, they must not silently collapse into one unmanaged operational file set.

When a change affects shared artifacts or derived outputs, confirm that:
- each shared artifact set still points to the intended generalized document set
- derived outputs are built from the intended shared source set
- no parallel shared artifact set silently keeps obsolete or conflicting meaning

## Mandatory Synchronization Check
When artifact structure changes, the agent must explicitly review whether updates are needed in:
- `AGENTS.md`
- `HARNESS_CONSTITUTION.md`
- `docs/harness/*`
- `plans/tech-debt-tracker.md`
- other shared artifact sets or derived outputs when they exist

If any related artifact is intentionally not updated, that decision must be visible and justified.

When a governing document is split, renamed, moved, or replaced, update the affected read paths, ownership references, and synchronization points before treating the change as complete.

## Generated Or Derived Artifacts
Generated or derived repository artifacts should be added only when they are:
- important to repeated agent work
- versionable in a stable form
- difficult to recover reliably from external context alone

Until those conditions hold, do not introduce a dedicated generated-artifacts structure.

When the repository must later compare against an earlier shared harness version, a stable baseline record may live in:
- `references/SHARED_HARNESS_BASELINE.md`

## Maintenance Rule
If artifact naming becomes inconsistent or directories multiply without clear class boundaries, treat that as governance drift and repair it before adding more structure.

Repeated failures to classify, synchronize, or separate artifact structures should themselves be recorded as promotion candidates for stronger enforcement.
