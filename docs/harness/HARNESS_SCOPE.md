# Harness Scope

## Purpose
This document defines the scope boundaries for the current project.

## Current Status
- Document status: `draft`
- Implementation status: `blocked until clarified`

## Status Transition Rules
- `draft`: starter-kit baseline state before project-specific facts are added
- `incomplete`: some project facts are known, but the law layer still cannot govern implementation safely
- `baseline ready`: the project has a coherent first-release boundary and the minimum law categories are materially filled in
- `ready for initial implementation`: the current first-release boundary is specific enough that implementation may begin without violating the documentation gate

## Known
- This repository is using the Harness document structure.
- Scope must be clarified before implementation begins.

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
- optional complexity that does not help establish a stable baseline

## Implementation Readiness Rule
Open questions may remain after this document is updated.

Initial implementation may still begin if those open questions do not invalidate the current first-release boundary.

Implementation must remain blocked if unresolved scope questions could materially change what the first release is supposed to do.

## Next Update Trigger
Update this document when:
- the project goal becomes clearer
- scope boundaries are confirmed or changed
- a new request expands or narrows the governed target
