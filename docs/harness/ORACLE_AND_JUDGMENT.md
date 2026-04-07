# Oracle And Judgment

## Purpose
This document defines how work in the current project is judged as acceptable, incomplete, or incorrect.

## Current Status
- Document status: `draft`
- Implementation status: `blocked until clarified`

## Status Transition Rules
- `draft`: starter skeleton state before project-specific judgment rules are added
- `incomplete`: some judgment rules exist, but implementation still lacks a safe oracle
- `baseline ready`: the project has both a usable structural oracle and an initial behavioral oracle for the first-release boundary
- `ready for initial implementation`: the current acceptance and verification baseline can judge the first release without hidden gaps

## Known
- Early judgment is based on harness readiness before product correctness.
- Implementation is blocked until the law layer contains enough criteria to govern the work.

## Unknown
- product-specific acceptance thresholds
- domain-specific correctness rules
- regression boundaries

## Assumptions
- The current project is still establishing its first governed baseline.
- Structural document completeness is always required, even after the project gains a behavioral oracle.

## Open Questions
- What will count as success for the product or system itself?
- What evidence will later prove the implementation is correct?
- What regression boundaries matter first?

## Blocked Until Clarified
- implementation that claims correctness without an explicit oracle
- pass or fail decisions that rely on undocumented judgment rules

## Structural Oracle
The structural oracle checks whether the governed document system is sound:
- the minimum law-layer files exist
- `Known`, `Unknown`, and `Assumptions` are separated
- implementation blockers are explicit
- open questions are visible

The structural oracle remains active throughout the project and does not disappear when product behavior becomes clearer.

## Behavioral Oracle
The behavioral oracle checks whether the project's first-release product or system behavior is correct.

Before the first-release boundary becomes materially known, this oracle may remain partial.

Once the project can describe what the first release is supposed to do, the behavioral oracle must be written in terms of concrete product or system behavior rather than document existence alone.

Document existence alone is never a sufficient behavioral oracle.

## Oracle Emphasis Rule
Early work may rely more heavily on the structural oracle.

Once the first-release boundary is materially known, judgment must use both:
- the structural oracle for governance integrity
- the behavioral oracle for product or system correctness

## Instance Oracle Preservation Rule
When this starter skeleton is specialized into a project-specific law set, behavioral oracle content should become more concrete and product-facing.

That specialization must extend the oracle system rather than collapsing it.

Project-specific oracle writing must preserve, unless a higher-order governing rule explicitly changes them:
- the continued existence of the structural oracle
- the requirement that behavioral oracle content be concrete once the first-release boundary is materially known
- the rule that document existence alone is not a sufficient behavioral oracle
- the expectation that both oracle layers remain usable for later review and implementation judgment

These are mandatory carry-forward protections for project instances and must remain explicitly readable in the resulting project law document rather than only implied through local prose.

## Acceptance Criteria
A change is acceptable when:
- it preserves the governing hierarchy
- it makes missing information explicit rather than hiding it
- it does not bypass the documentation gate
- it does not treat assumptions as facts
- it keeps both structural and behavioral judgment explicit enough for another agent to follow

## Documentation Gate Judgment
Implementation is documentation-incomplete until the law layer defines:
- scope boundaries
- contract expectations
- judgment criteria
- failure classification
- execution flow and regression expectations

## Implementation Readiness Rule
Implementation may begin when the project has:
- a usable structural oracle that another agent could follow with reasonable consistency
- a usable behavioral oracle for the current first-release boundary

Open questions may remain if they do not prevent correct judgment of the current first-release boundary.

Implementation must remain blocked if correctness still depends on undocumented assumptions or if either oracle is too weak to judge the first-release boundary safely.

## Change Judgment
When a request arrives, judge it in this order:
1. Does it change governance meaning?
2. Does it require law-document updates?
3. Is implementation still blocked?

## Consistency Judgment
If documents conflict, restore consistency at the layer that owns the meaning before proceeding.

## Regression Expectations
Regression includes:
- implementation starting before the documentation gate is satisfied
- assumptions being treated as facts
- open questions disappearing without resolution
- lower-order documents overriding higher-order documents

## Evidence Of Completion
Initial governance completion requires:
- the minimum law-layer set exists
- unresolved items are visible
- implementation status is accurately stated

Once the first-release boundary is materially known, completion evidence must also include a product-facing oracle that another agent could apply without inferring missing behavior.

## Review Checks
- Are judgment rules explicit enough for another agent to follow?
- Are blockers still real or already resolved?
- Does the project now need more specific oracle rules?
- Is the structural oracle still intact?
- If the first-release boundary is already known, is the behavioral oracle explicit enough to judge product or system behavior?

## Minimum Verification Baseline
At minimum, verification should cover:
- one valid core flow inside the first-release boundary
- one invalid or unsupported input path when such a path exists
- one state-change check that proves the main outcome really changed
- one persistence, reload, repeat-run, or equivalent continuity check when that behavior matters
- one check that out-of-scope behavior was not silently introduced

The minimum verification baseline should be written so that another agent can test concrete behavior, not only document completeness.

## Regression Strategy
Regression expectations should identify:
- the smallest behavior set that defines the first release
- the behaviors most likely to break after refactoring or extension
- the stored, repeated, or cross-step outcomes that must remain stable
- the points where a change should force the verification baseline to expand

## Structured Expansion Judgment
The project should judge its documentation structure as insufficient when:
- important decisions no longer fit cleanly inside the current law layer and short execution map
- complex work proceeds without a versioned plan
- important references are repeatedly needed but live only outside the repository
- the same review rule recurs often enough that prose-only enforcement becomes unreliable

When one or more of these conditions appears, the correct response is to expand the repository structure rather than overloading `AGENTS.md`.

## Next Update Trigger
Update this document when:
- product-specific judgment becomes clearer
- regression expectations change
- implementation readiness changes
