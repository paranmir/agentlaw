# Input Output Contract

## Purpose
This document defines the minimum working contract for requests, inputs, outputs, and document-update expectations in the current project.

## Known
- The current project must follow a document-first workflow.
- New requests must be checked for document impact before implementation begins.

## Unknown
- stable product requirements
- authoritative external inputs
- required interfaces
- expected delivery artifacts beyond the initial law layer

## Assumptions
- The current request and repository contents are the initial available inputs.
- More project-specific inputs will be added later.

## Open Questions
- What inputs are mandatory before implementation can begin?
- What outputs count as the first meaningful project deliverables?
- Which interfaces or external dependencies must be governed?

## Blocked Until Clarified
- implementation that depends on undefined inputs or outputs
- automation that assumes contracts not yet written down

## Required Inputs
- the current request
- `HARNESS_CONSTITUTION.md`
- `AGENTS.md`
- the current `docs/harness/*` law documents

## Available Inputs
- repository root
- current user request
- current governance documents

## Missing Inputs
- product goal
- user roles
- interface boundaries
- acceptance targets

## Input Interpretation Rules
Every new request must first be interpreted as a possible governance change.

If a request changes scope, contract meaning, judgment logic, failure handling, execution flow, or regression expectations, the law layer must be updated first.

## Change Classification Rule
Before making meaningful changes, classify the request as one or more of:
- `content-only`: changes local content without changing artifact structure or governance routing
- `structure-affecting`: changes repository structure, artifact classes, or directory usage
- `shared-artifact-affecting`: changes shared artifact sets or how derived outputs relate to them
- `governance-affecting`: changes constitutional or law-layer meaning

This classification must be made before implementation or restructuring work proceeds.

## Expected Outputs
- clarified law-layer documents
- visible open questions
- explicit implementation blockers when criteria are missing
- governed implementation only after the documentation gate is satisfied

## Output Quality Requirements
Outputs must:
- distinguish `Known`, `Unknown`, and `Assumptions`
- avoid silent gaps in critical fields
- make blockers explicit
- keep `AGENTS.md` short and operational

## Synchronization Rule
When a change is `structure-affecting`, `shared-artifact-affecting`, or `governance-affecting`, the agent must check whether related governing artifacts also require updates.

At minimum, check the relevant combination of:
- `HARNESS_CONSTITUTION.md`
- `AGENTS.md`
- `docs/harness/*`
- `plans/tech-debt-tracker.md`
- `references/*`
- derived outputs or parallel shared artifact sets when they exist

A change is not complete if it leaves known related governing artifacts outdated without explicitly recording the deferred impact.

## Minimum Documentation Gate
Implementation must not begin until the law layer documents:
- scope boundaries
- required inputs and expected outputs
- judgment criteria
- failure classification
- execution flow and regression expectations

## Implementation Readiness Rule
Implementation may begin when the current project has:
- a stable first-release boundary
- a usable input and output contract for that boundary
- explicit judgment criteria
- explicit failure handling expectations
- a documented runner flow and regression strategy

Open questions may remain if they do not materially change the current runtime contract.

Implementation must remain blocked if unresolved questions change what inputs, outputs, or interfaces the first release depends on.

Implementation does not need every runtime detail to be final if:
- the first-release meaning is already stable
- the remaining runtime choice stays within the documented boundary
- the current runner flow can still describe how the chosen behavior will be exercised and checked at the present level of detail

Implementation should remain blocked on runtime details only when those details materially change what the first release is, how correctness is judged, or what round-trip behavior must exist.

## Runtime Input Contract
The first implementation must document:
- the primary user or system inputs
- the minimum valid input shape or action form
- the invalid or unsupported inputs that must be rejected, prevented, or deferred
- any persisted or external inputs that must be loaded before the main flow works

## Runtime Output Contract
The first implementation must document:
- the visible or observable outputs of the main flow
- the persisted or transmitted outputs that must remain correct after execution
- the expected result when no usable data exists yet
- the expected result when invalid input is rejected or blocked

## Minimal Execution Flow
1. Read the governing documents.
2. Judge document impact before coding.
3. Update the law layer when meaning changes.
4. Keep implementation blocked until the minimum documentation gate is satisfied.

## Runner Flow
The default runner flow should document:
1. how the project is started in its current working mode
2. how a core valid input is exercised
3. how the main state change is observed
4. how the project is stopped, reloaded, or re-entered when persistence or continuity matters
5. how the expected result is re-checked after that round-trip

When the project has multiple distinct features or entry points, the runner flow must cover each feature — not just the primary one. For each feature, document:
- its distinct inputs and how they are provided
- its dependencies on other features or on shared state produced by other flows
- any simplifying assumptions or fixed parameters that limit the feature's behavior compared to what the exposed interface suggests

If the project grows more complex, this content may later move into a separate `RUNNER_FLOW.md`.

Early runner flow may be expressed at the currently governed level of runtime detail. It does not need final toolchain specificity before first implementation unless the runtime choice materially changes product meaning or judgment.

## Regression Strategy
The default regression strategy should document:
- the smallest set of behaviors that must keep working after any meaningful change
- the invalid inputs or edge cases that must stay rejected or controlled
- the persisted, repeated, or round-trip behaviors that must remain stable
- the conditions that require the regression set to expand

If the project grows more complex, this content may later move into a separate `REGRESSION_STRATEGY.md`.

## Expansion Triggers
The repository should add structured planning or reference artifacts when:
- a task is too large to track safely inside a short execution map
- a multi-step change needs progress tracking or decision logs
- important external or internal references must remain searchable in the repository
- generated or derived facts become important to repeated agent work

At that point, the project may add:
- `plans/active/*`
- `plans/completed/*`
- `plans/tech-debt-tracker.md`
- `references/*`

These additions support the law layer; they do not replace it.

## Mechanical Enforcement Promotion
When the same constraint is checked repeatedly in review, the project should record it as a promotion candidate for lint, CI, structural tests, or equivalent automated enforcement.

## Completion Gate For Structural Changes
A `structure-affecting`, `shared-artifact-affecting`, or `governance-affecting` change should be treated as incomplete until:
- the change classification is explicit
- the affected governing artifacts have been reviewed for synchronization impact
- required updates are applied or deferred impact is explicitly recorded
- no known artifact-set confusion remains

## Next Update Trigger
Update this document when:
- required inputs become clearer
- expected outputs or interfaces change
- the implementation gate changes
