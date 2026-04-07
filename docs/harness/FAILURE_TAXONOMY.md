# Failure Taxonomy

## Purpose
This document classifies harness-relevant failure modes in the current project and defines the expected recovery direction.

## Current Status
- Document status: `draft`
- Implementation status: `blocked until clarified`

## Status Transition Rules
- `draft`: starter skeleton state before project-specific failure patterns are added
- `incomplete`: some failure classes are known, but recovery expectations are still too weak to guide implementation safely
- `baseline ready`: the first-release failure classes and recovery directions are clear enough to govern initial work
- `ready for initial implementation`: the current failure taxonomy is specific enough to catch likely first-release failures and guide recovery

## Known
- Early-stage projects can fail before code exists.
- Missing governance is itself a failure mode.

## Unknown
- project-specific incident patterns
- later-stage severity thresholds
- domain-specific failure classes

## Assumptions
- Early failure classes should focus on scope, contract, oracle, and premature implementation.

## Open Questions
- Which failure classes will matter once implementation starts?
- What failures should later trigger regression checks or automation?

## Blocked Until Clarified
- implementation that depends on undefined failure handling
- operational commitments that assume a mature taxonomy already exists

## Failure Classes
- `scope undefined`: the governed target or scope boundary is unclear
- `contract missing`: required inputs, outputs, or interface expectations are not defined
- `oracle missing`: correctness cannot be judged explicitly
- `implementation started too early`: coding begins before the documentation gate is satisfied
- `assumptions treated as facts`: temporary guesses are used as confirmed truth

## Severity Interpretation
- `critical`: governance failure that invalidates the current work path
- `high`: failure that should block implementation immediately
- `medium`: failure that distorts planning or review but is still recoverable before implementation
- `low`: wording or clarity issue that does not yet alter behavior

## Recovery Expectations
- clarify the owning document layer
- update the missing or incorrect law document
- re-state blockers when implementation is still not allowed
- do not patch over governance failures in lower-order documents

## Regression Review Expectations
Review for repeated failures such as:
- unresolved scope drift
- missing contract updates
- undocumented oracle changes
- premature implementation
- assumptions silently promoted to facts

## Runner Failure Checks
The runner flow should explicitly check for:
- failure to perform the main in-scope flow successfully
- failure to preserve the expected result across a reload, restart, repeat run, or equivalent round-trip when continuity matters
- failure to target the intended entity, record, or state during mutation
- failure to reject, block, or contain invalid input when such handling is part of the contract
- failure caused by silent scope expansion or undocumented behavior changes

## Structure Expansion Failures
The project should also classify these as governance failures when growth makes them relevant:
- `plan gap`: complex work is proceeding without a repository-tracked plan
- `reference gap`: important reference knowledge exists only outside the repository
- `enforcement gap`: a repeated invariant remains prose-only after it is known to be drift-prone
- `agents overload`: `AGENTS.md` accumulates durable knowledge that should have moved into structured artifacts

## Next Update Trigger
Update this document when:
- new recurring failure classes appear
- severity interpretation changes
- recovery expectations become more specific
