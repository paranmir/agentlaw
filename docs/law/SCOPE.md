# Harness Scope

## Purpose
This document defines the scope boundaries for the current project.

## Known
- This repository is using the Harness document structure.
- Scope must be clarified before implementation begins.
- Harness includes default memory and continuity subsystems when applied to a repository.
- Continuity and memory are official Harness capability classes, but runtime memory state is derived context rather than law.

## Unknown
- final product or system goal
- primary user or operator
- feature boundaries
- non-goals that should remain outside the project

## Assumptions
- The current project is in an early setup or definition stage.
- The law layer must be completed before meaningful implementation begins.

## Open Questions
- What is the project trying to build or govern?
- What is explicitly inside the first working scope?
- What is explicitly outside the first working scope?
- Are there repository or subsystem boundaries that matter immediately?

## Blocked Until Clarified
- implementation work that depends on unresolved scope
- feature commitments that require explicit in-scope or out-of-scope decisions

## Governed Target
`new project repository`

## In Scope
- harness setup
- project-definition work
- requirement clarification
- documentation needed to govern implementation

## Out of Scope
- production implementation before the law layer is clarified
- undocumented feature work
- silent expansion of scope without document updates
- treating memory or installer metadata as project authority above file-based governance

## Non-Goals
- choosing a stack by default
- fixing architecture before the project goal is clear
- treating assumptions as confirmed scope

## Repository Or System Boundary
This document governs the current repository as the active project boundary unless a narrower subsystem boundary is later documented here.

## First Release Boundary
The first release should define:
- the smallest useful user-facing or system-facing outcome
- the minimum in-scope feature set needed for that outcome
- the most important excluded features that must stay out of scope for now

The first release does not need to define:
- later-stage feature expansion
- advanced integrations that are not required for the first useful outcome
- advanced complexity that does not help establish a stable baseline

## Continuity And Memory Boundary
Memory and continuity support are default Harness subsystems. This document must clarify how they relate to the current project scope.

Rules:
- memory runtime state is lower-authority derived continuity
- memory must not silently expand project scope or override repository files
- runtime memory paths belong in artifact rules before they become normal practice

## Implementation Readiness Rule
Open questions may remain after this document is updated.

Initial implementation may still begin if those open questions do not invalidate the current first-release boundary.

Implementation must remain blocked if unresolved scope questions could materially change what the first release is supposed to do.

## Next Update Trigger
Update this document when:
- the project goal becomes clearer
- scope boundaries are confirmed or changed
- a new request expands or narrows the governed target
