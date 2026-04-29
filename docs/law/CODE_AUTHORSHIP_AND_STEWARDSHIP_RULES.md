# Code Authorship And Stewardship Rules

## Purpose
This document defines how agents must write, generate, modify, refactor, delete, review, and verify code without creating avoidable technical debt.

It operationalizes the constitutional rule that code maintainability and testability are part of correctness.

## Scope
These rules apply whenever an agent works on code, including:

- first implementation
- generated code
- feature work
- bug fixes
- refactoring
- deletion
- public API, CLI, MCP, schema, or protocol changes
- tests and test fixtures
- code review findings that require code judgment

They apply from the first implementation. An agent must not create avoidable technical debt and defer basic structure, reuse, or testability to unspecified future cleanup.

## Behavioral Oracle Rule
Before writing or changing tests, agents must identify the behavioral oracle: the source that defines expected behavior.

Oracle priority is:

1. The user's explicit current request.
2. `AGENTLAW_CONSTITUTION.md`.
3. Root control documents.
4. `docs/law/*` law documents.
5. Public contracts, API references, MCP tool references, CLI help, schemas, and documented return shapes.
6. Existing committed tests and existing public behavior, when characterization is the goal.
7. Domain standards, protocols, invariants, and compatibility requirements.
8. Internal implementation details only when the task is explicitly about internal structure.

Tests must assert behavior promised by the behavioral oracle, not the agent's preferred implementation.

If the oracle is missing, ambiguous, or contradictory, the agent must not invent durable behavior silently. It must ask, record an explicit assumption, or limit the test to characterization of existing public behavior.

Existing behavior is not a valid oracle when it conflicts with higher-authority documents or when the task is to fix that behavior.

## Pre-Edit Code Work Gate
Before non-trivial code edits, the agent must state or record:

- the affected surfaces
- the behavioral or mechanical oracle
- the focused verification target
- whether the work is high-risk
- the smallest safe slice
- explicit non-goals or deferred risks

This gate applies to first implementation, modification, refactoring, deletion, and generated code.

The gate does not require a long plan for trivial mechanical edits, but the agent must still run or identify the smallest relevant verification.

When code work is non-trivial, the planning workflow in
`docs/law/PLANNING_AND_REVIEW_RULES.md` also applies. The pre-edit gate supplies
the code-specific affected surfaces, oracle, verification target, risk, slice,
and non-goals that the revised plan must carry.

## Test-Anchored Development Rule
All non-trivial code work must be anchored by executable verification.

For new behavior, prefer failing tests before implementation. If tests are written alongside implementation, the agent must still run the relevant tests and make the expected behavior explicit.

For bug fixes, add a regression test that fails before the fix unless an existing focused test already fails and precisely covers the bug.

For refactoring, add or identify characterization tests before changing structure. Refactoring must not proceed without executable coverage of the behavior being preserved, unless the refactor is purely mechanical and independently verifiable.

Existing coverage is adequate only when it exercises the behavior affected by the change. Broad test-suite existence is not sufficient.

Focused verification must be executable or mechanically checkable. Manual inspection alone is not sufficient for non-trivial code work.

### Publish Gate Oracle Pairing

Before any artifact publish — PyPI / package-registry upload, GitHub release tag, or scaffold mirror push — the agent must explicitly enumerate every file in scope (defined below) that has been modified since the prior published version, and confirm that each scope category has corresponding test or verifier coverage that did not exist at that prior version. The enumeration must be visible in the change record (plan body, save log, or commit message), not assumed.

The gate applies to two categories, with different "test" satisfiers:

- **Runtime source code** under the published package tree (e.g. `src/<package>/*.py`, excluding the bundled scaffold and `__init__.py`-only edits). Pairing satisfier: a modified file under `tests/`. The mechanical check is `_test_publish_oracle_pairing`.
- **Harness governance content** that ships to downstream projects: the law layer under `docs/law/`, contracts under `docs/contracts/`, root control documents (`AGENTLAW_CONSTITUTION.md`, `HARNESS_*_TOOL.md`, `AGENTS.md`), the public seed under `publish-repo/`, and the bundled scaffold copy under `src/<package>/scaffold/`. Pairing satisfier: a modified file under `tests/` **or** a modified `src/<package>/verify_cmd.py` (counting a new or updated verifier check as the oracle for a new or updated mechanical rule). The mechanical check is `_test_harness_content_oracle_pairing`.

The harness-content category exists because every fresh agent reads the law layer on every session restore; a textual change there changes governance behavior across every downstream project that installs the kit, so the §Test-Anchored Development Rule extends past Python source. A refactor that touches only mirrored copies (e.g. `publish-repo/` or scaffold without the authoring source under `docs/law/`) still trips this gate unless paired — by design, because mirror drift is the classic governance failure mode the gate exists to catch.

A single ad-hoc smoke invocation against a freshly built wheel does not satisfy this clause for new branching logic. Smoke checks startup integrity; the oracle checks per-branch behavior.

Discovering a missing oracle for the first time after publish-readiness checks have already started is itself a fail-the-publish condition: the publish must pause until the pairing is restored as committed tests (or, for harness-content changes, a committed verifier check) in the same change-set that introduced the new behavior. Bumping the package version solely to ship the missing oracle later is not an acceptable substitute, because v0.X.Y on a registry remains addressable forever and downstream installers may pin to it.

Out-of-scope edits that legitimately do not need pairing — for example, tightening a comment in `tests/`, touching a memory log, or updating a plan body — are detected by the gate's filters and do not trigger the failure condition. The gate's heuristics produce some false positives (a purely textual law clarification, with no new mechanical aspect, must still be paired with a trivial `tests/` or verifier-check edit acknowledging the change); this is the documented price of mechanical enforcement at the change-set granularity.

Executable or mechanically checkable verification includes:

- unit tests
- integration tests
- CLI or MCP round-trip tests
- schema migration tests
- type checks
- linters
- snapshot checks with a stable oracle
- browser, screenshot, or interaction checks where UI behavior is the changed surface
- harness verifier or equivalent structural checks when governance artifacts change

## Non-Trivial Code Work
Non-trivial code work includes:

- new behavior
- behavior changes
- bug fixes with regression risk
- refactors
- persistence or schema changes
- concurrency or background jobs
- public API, MCP, CLI, or protocol surface changes
- filesystem writes
- external process, network, model, or embedding calls
- security, authorization, scope, or safety boundaries
- changes touching shared helpers or cross-component contracts

Trivial exceptions are allowed only for non-behavioral or purely mechanical edits. The agent must name the reason and still run the smallest relevant verification.

## High-Risk Code Rule
High-risk code includes:

- persistence and schema migrations
- concurrency, background workers, locks, and job status
- public API, MCP tool, CLI, or transport surfaces
- filesystem writes and destructive operations
- security, authorization, permission, or scope checks
- external process, network, model, or embedding calls
- authority, memory, or governance-boundary behavior

High-risk code has no trivial exception. It requires focused executable tests unless an existing focused test already covers the changed behavior.

High-risk code work must not be implemented as one broad one-shot change unless the user explicitly requests that risk and the agent records the affected surfaces, oracle, rollback or recovery path, and residual risk before editing.

For high-risk work, oracle deferral is allowed only with explicit user approval or a recorded blocking reason.

## First Implementation Rule
Before creating a new function, class, module, helper, service, parser, adapter, or abstraction, inspect nearby code for existing patterns and reusable boundaries.

If no local pattern exists, choose the smallest coherent structure that keeps responsibilities separated and behavior testable.

Disposable spikes or prototypes must be explicitly scoped as temporary. Temporary code must not be promoted into durable code without satisfying these rules.

## Minimal Coherent Design Rule
Prefer the smallest coherent design that satisfies the task and keeps likely next changes understandable and testable.

Avoiding technical debt does not mean maximizing abstraction.

New abstractions are allowed only when they remove real duplication, isolate a real dependency, clarify a responsibility boundary, or stabilize a repeated change axis under current requirements.

## Enabling Refactor Rule
If implementing the requested behavior would require adding more branching, duplication, or responsibility mixing to an already unclear area, the agent must first add or identify tests, then perform the smallest enabling refactor before adding behavior.

An enabling refactor must preserve behavior and stay scoped to the boundary needed for the requested work.

Broad rewrites require an approved plan and stronger characterization coverage before implementation.

## Structure And Dependency Rule
When writing or modifying code, agents must keep structure obligations visible:

- A module, class, or function should have one clear primary responsibility.
- New logic should reuse existing local boundaries before adding another helper or abstraction.
- External systems such as databases, filesystems, processes, network calls, model calls, embedding calls, and clocks should be isolated behind testable boundaries when they affect behavior.
- Public interfaces should stay small and task-focused.
- Dependency direction should keep domain behavior testable without requiring real external systems where fakes or controlled fixtures are appropriate.

## Review-Friendly Implementation Context

Agents must make non-trivial code understandable to future reviewers. Context must be preserved through self-documenting code, formal API documentation, and reasoning-critical comments. The project mandates strict adherence to the established documentation standard of the target language (e.g., Google Style for Python, TSDoc for TypeScript, Rustdoc for Rust).

### 1. Mandatory Type Contracts and Self-Documenting Code
- All language surfaces that support static typing or type hints must be fully annotated (e.g., return types, parameter types, struct/class attributes).
- Explicitly declare void returns if the language standard expects it.
- Variables and functions must have descriptive, unambiguous names. Code boundaries must document themselves structurally before relying on comments.
- **Prohibited:** Redundant comments that merely restate type signatures or variable names.

### 2. Formal API Documentation Specification
All public modules, classes, and functions—as well as non-trivial internal boundaries—must carry formal, tooling-compatible API documentation.
- **Summary Line:** The first sentence must be a concise summary in the imperative mood.
- **Detailed Description:** Provide context on *why* this abstraction exists and explicitly define any invariants, critical edge cases, or failure modes.
- **Parameters/Arguments:** List and describe what each parameter controls or affects. Do not repeat the type if the language signature already enforces it.
- **Return Values:** Describe the semantic meaning of the return value, not just its type.
- **Errors/Exceptions:** Explicitly enumerate error states, exceptions raised, and the conditions that trigger them.

### 3. Reasoning-Critical Inline Comments
Inline comments must explain *why* code does something, not *what* it does.
- Comments must document authority boundaries, security boundaries, write ordering, schema migration constraints, platform-specific workarounds, and non-obvious failure handling.
- **Prohibited:** Routine operational comments (e.g., "Initialize the connection" or "Close the file") unless the operation involves counter-intuitive side effects.
- **Prohibited:** Self-narration of the code's revision history. Comments must not become implementation history, stale TODOs, work logs, or duplicated active plans. Specific anti-patterns: "renamed from X on YYYY-MM-DD", "added on YYYY-MM-DD as part of subplan Z", "this used to be Y until the field rename", "the earlier formulation returned a tuple but was changed", or any equivalent in-comment changelog narration. History lives in git blame, plan documents, memory log entries, and tracker entries — not in comment bodies. A reader who genuinely needs "why was this renamed?" follows pointers into those layers, not the docstring. This is the code-side equivalent of `docs/law/REPOSITORY_ARTIFACT_RULES.md` §"Self-Narration Prohibition" for governed artifact bodies.
- If behavior changes, the agent must update or remove affected comments in the same commit.

## Implementation Design Context
Non-trivial implementation work must create or update implementation context before completion.

A short design section inside an active plan is sufficient for small scoped changes.

A separate design/reference document is required when the work affects persistence or schema, public API/CLI/MCP behavior, package/install behavior, security boundaries, cross-module control flow, broad refactoring, or data models that future agents must understand.

Active plans are execution artifacts. Durable design knowledge that remains useful after the work is complete should live in a reference/design document, not only in an active plan.

Implementation context must be updated, archived, or marked outdated when the implementation diverges from the design.

Prefer ASCII diagrams or tables for simple module, state, or data flows. Mermaid is allowed when ASCII would be harder to read or maintain, such as complex state machines, sequence flows with branching or retries, ERDs with several related tables, or dependency graphs with many edges. Diagrams must support review and must not be decorative.

## Code Architecture Map
The `project-overview` reference must include a "Code architecture map" section that lets a reader, within five minutes, answer:

1. Structure: what modules, packages, or classes make up the project and how they depend on each other.
2. Flow: when a user-facing entry point (CLI command, HTTP handler, MCP tool, event handler, and so on) runs, which code path executes.

Slot menu. Each diagram in the section uses exactly one slot and carries a heading naming its slot so the reader knows the shape of graph they are reading:

- Module dependency (structural) — Mermaid `graph TD`; nodes are modules or packages, edges are import relationships.
- Entry-point call graph (flow) — Mermaid `graph TD` or `sequenceDiagram`; one subdiagram per user-facing entry point; nodes are functions or classes, edges are call or invocation.
- Class diagram (structural) — Mermaid `classDiagram`; only for subsystems with real object-oriented structure, not for the whole project by default.
- State diagram (flow) — Mermaid `stateDiagram-v2`; for subsystems whose behavior is dominated by lifecycle states.
- Data flow (flow) — Mermaid `graph LR`; for subsystems that are primarily data-transformation pipelines.

Requirements:

- At least one structural slot and at least one flow slot. Library-only projects without user-facing entry points may omit the flow slot, and must record that exemption in a single sentence next to the slot-selection rationale.
- Slot-selection rationale: one short paragraph stating which slots the project uses and why, so the choice itself is reviewable.
- Map scope: the section declares a `Map scope:` block listing the repository paths the map covers as glob patterns (one pattern per bullet). Files outside the declared scope are not expected to appear in the map.
- Density cap: each diagram contains at most fifteen nodes and at most twenty-five edges. When a diagram would exceed the cap, split it via boundary nodes (dashed-border placeholders that appear in both the parent and child diagrams) and add a node index table identifying the primary-home diagram for each node.
- Format: Mermaid only. ASCII diagrams are not valid slots for this section, because uniform rendering keeps cross-diagram references unambiguous.
- Update obligation: when a change within the declared Map scope adds, removes, renames, or reshuffles modules, classes, or user-facing entry points, the same commit must update the affected diagrams. Layer 2 enforcement: the harness verifier checks scoped-file freshness against the `project-overview` reference file and fails when any scoped file appears newer than the reference. The specific freshness signal is an implementation detail of the verifier and may evolve; the current method is documented in the verifier source.

The obligation applies to the `project-overview` reference role named in `docs/law/REPOSITORY_ARTIFACT_RULES.md`. Projects that have not yet declared a `Map scope:` block pass the Layer 2 check silently, so newly initialized targets are not forced into FAIL until they opt in by populating the section.

## SOLID Interpretation
SOLID is a named source for the structure obligations above, not a requirement to introduce classes, inheritance, interfaces, factories, or pattern-shaped code.

Apply SOLID during design, implementation, refactoring, and review whenever code structure or dependencies are involved:

- Single Responsibility: keep each unit centered on one primary reason to change.
- Open-Closed: avoid designs that require editing many unrelated places for one coherent behavior change.
- Liskov Substitution: when implementations are substitutable, preserve caller expectations.
- Interface Segregation: do not force callers to depend on methods, fields, or data they do not use.
- Dependency Inversion: isolate external effects and depend on stable boundaries where that keeps behavior testable.

These principles guide judgment. They do not justify speculative abstraction.

## Design Pattern Use Rule
Established design patterns may be used when they reduce actual complexity, isolate a real dependency, or stabilize a repeated change axis under current requirements.

They are prohibited when they add ceremony without reducing current complexity.

Agents must not add factories, interfaces, base classes, visitors, observers, strategies, or other pattern-shaped code only because a pattern name exists.

## Anti-Patterns
The following are code-authorship failures unless explicitly justified by the task and covered by appropriate verification:

- adding one-off helpers without checking local equivalents
- mixing transport, persistence, parsing, policy, formatting, and external calls in one growing function
- writing tests against private implementation details when public behavior is the real contract
- broad snapshot tests without a stable oracle
- mocks that verify the mock setup rather than behavior
- broad rewrites without characterization tests
- adding factories, interfaces, base classes, or strategies before a real repeated change axis exists
- treating full-suite success as proof that the changed behavior was tested
- claiming manual inspection as sufficient verification for non-trivial code work

Reasonable local duplication may be acceptable when it is clearer than abstraction and does not create a repeated maintenance burden.

## Verification Requirements
For code work, verification should proceed from focused to broad:

1. Run the smallest committed tests or checks that directly exercise the changed behavior.
2. Run additional integration or boundary tests when the change crosses components or public contracts.
3. Run the repository's whole-repo verification baseline before completion unless the user explicitly defers it.

When governance artifacts change, run the harness verifier or equivalent structural check.

When generated artifacts or packaged templates exist in the project, update them through the project's documented sync path before verification.

## Completion Evidence
Before declaring code work complete, the agent must report:

- the behavioral oracle used
- the focused verification run
- any broader verification run
- any test gaps, skipped slow tests, or accepted residual risks
- any enabling refactor performed before the requested behavior


If verification cannot be run, the agent must say why and identify the remaining risk.
