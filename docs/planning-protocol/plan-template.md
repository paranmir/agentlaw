# Plan Template

## Purpose

This document is the canonical structural skeleton for repository-tracked plans.
The agent copies this template and fills its sections when authoring a plan.

The template carries **structure only**. Rules about classification, review
obligations, persona selection, and skip conditions live in
`docs/law/PLANNING_AND_REVIEW_RULES.md` and the other operational files under
`docs/planning-protocol/`. Cross-references in the template point to those
authorities; the template itself does not encode rule semantics.

## Layered Authority

When using this template:

- Whether a plan is needed at all is decided by `task-classification.md`
  § Quick Gate.
- Whether the plan is trivial or non-trivial is decided by
  `task-classification.md` § Non-Trivial Triggers.
- Which sections must be filled depends on the trivial/non-trivial decision
  per `docs/law/PLANNING_AND_REVIEW_RULES.md` § Trivial Work Boundary.
- Which personas review the plan is decided by `review-method.md`
  § Selection Rule.

## Section-Fill Profile

- **Trivial plan-required work**: fill `Status`, `Classification`,
  `Intent`, `Scope`, and `Plan Review Evidence` (Trigger Coverage Verifier
  pass only). Other sections may be marked N/A.
- **Non-trivial plan-required work**: fill all applicable sections. Sections
  for unmarked conditional domains may be marked N/A.
- **Plans that introduce or revise rule-system artifacts**: include a
  `Bootstrap Transitional Declaration` section per
  `docs/law/PLANNING_AND_REVIEW_RULES.md` § Bootstrap Transitional Exemption.

The template below uses block comments to indicate which sections are
universal, conditional, or evidence-only. Comments are removed from the
final plan body.

---

## Template Body

Copy from here downward into a new plan file under `docs/plans/active/`.

```markdown
# [Plan Title]

## Status

- Status: draft / active / completed
- Date authored: YYYY-MM-DD
- Author/agent context: [optional, free-form]

## Bootstrap Transitional Declaration

[Include this section ONLY when the plan introduces or revises an artifact
under docs/law/PLANNING_AND_REVIEW_RULES.md or docs/planning-protocol/.
Otherwise omit. See PLANNING_AND_REVIEW_RULES.md § Bootstrap Transitional
Exemption for the required form.]

## Classification

- Task class(es): [from task-classification.md]
- Plan required: yes
- Reason: [why a plan is required for this task]
- Risk triggers matched: [cited triggers from task-classification.md
  § Non-Trivial Triggers; empty list if zero matches]
- Importance/Risk: trivial / non-trivial
- Primary output contract: [shape of the deliverable]
- State changes: [what mutates during execution]
- Freshness/source requirement: [time-sensitive inputs, if any]
- User gate required: yes / no, before what

## Domain Coverage

[Non-trivial plans only. Self-mark the conditional domains that apply.
Trivial plans may omit this section.]

- [ ] Security & Trust Boundaries
- [ ] Data Stewardship
- [ ] External Contract & Compatibility
- [ ] Time & Sequencing
- [ ] Cost, Resource & Performance
- [ ] Long-term Care
- [ ] Self-Application (rule/tooling-introducing plans)

[Universal domains — Intent, Scope, Premise, Risk, Verification, Confidence,
Continuity, Governance — always apply and do not need self-marking.]

## Intent

- Restated ask: [in the author's own words; user-confirmable form]
- Underlying need: [if literal ask might mask a deeper goal]
- Output artifact shape: [specific form, format, length]
- Acceptance criteria: [concrete verification action; no vague phrases]

## Scope

- Affected surfaces: [list every surface type the plan touches; bullet list
  with backticked paths/identifiers per docs/law/REPOSITORY_ARTIFACT_RULES.md]
- Stakeholders (segmented): [people/roles affected, by segment]
- Downstream consumers: [systems/artifacts/agents that depend on outputs]
- Indirect effects: [enumerated by category — related artifacts, stored
  state, alerts, environment, expectations]
- Temporal scope: one-time / ongoing / time-bounded
- Explicit non-goals: [out-of-scope list]

## Premise

- Facts: [each with verification artifact reference]
- Sources: [each with specific identifier + freshness threshold]
- Assumptions: [each with verification path — what would convert it to
  verified]
- Prerequisites: [tree of conditions that must hold for the plan to apply]
- Prior art: [search trace; what already exists]

[Conditional under Premise — fill only if relevant:]

- Source conflicts: [how conflicting sources are reconciled]
- Runtime / live state: [active probes for current state at execution time]
- Access-premise: [evidence of permissions/access being held]

## Confidence

- Confidence labels: [each non-trivial claim labeled verified / assumed /
  unknown / TBD, with basis]
- Undecided / TBD items: [decisions deferred, with unblocking conditions]

[Conditional under Confidence — fill if relevant:]

- Bounded knowledge: [scenarios that could invalidate the plan]
- Confidence-shift protocol: [escalation triggers when confidence falls]

## Risk

- Reversibility per step: [classification per step]
- User gates: [where, what asked]
- Rollback / recovery path: [per non-reversible step]
- Counterfactual fallbacks: [if X fails, then Y; cascade if Y fails]

## Verification

- Pre-action checks: [what to verify before execution]
- Post-action checks: [what to verify after]
- Verification strategy: [tests/oracles/adversarial cases matching risk]

## (Conditional) Security & Trust Boundaries

[Fill if marked in Domain Coverage.]

- Trust boundaries crossed:
- Sensitive identifier handling:
- Permission boundary (minimum required):
- Adversarial paths (system + user-side):
- Supply chain considerations:

## (Conditional) Data Stewardship

[Fill if marked.]

- Information integrity & migration:
- Privacy & retention:
- Information validity decay (drift):

## (Conditional) External Contract & Compatibility

[Fill if marked.]

- Public interface changes:
- Backward compatibility policy:
- Versioning strategy:
- Deprecation path: [if removing surface]

## (Conditional) Time & Sequencing

[Fill if marked.]

- Step ordering / dependency:
- Parallel/concurrent execution risks:
- Deadlines / expiry:

## (Conditional) Cost, Resource & Performance

[Fill if marked.]

- Resource cost estimate:
- Efficiency / responsiveness impact:

## (Conditional) Long-term Care

[Fill if marked.]

- Maintainability impact:
- Continued operability / observability:
- Documentation impact:

## Continuity

- Internal continuity (handoff state + decisions): [what next session needs]

[Conditional under Continuity — fill if relevant:]

- Persistent records to update: [external state to update]

## Governance

- Existing rules involved:
- Cross-rule consistency: [are referenced rules mutually consistent for
  this plan]

## Self-Application

- Rule-on-self status: [does the plan apply rules it introduces to itself]

[Conditional under Self-Application — fill if rule-introducing:]

- Bootstrap & transition: [transitional exemption declaration if invoked]
- Coherent examples: [rule examples follow the rule]
- Self-revisability protocol: [how the rule allows itself to be revised]

## Plan Review Evidence

[Filled in after persona review, not at draft time.]

- Review required: yes / no
- Review exemption reason: [if no]
- Personas considered: [list per deck source]
- Persona deck sources: [paths]

### Separate Persona Review Passes

[For trivial plans: only the Trigger Coverage Verifier pass appears here.]
[For non-trivial plans: one block per applied persona.]

#### [Persona name]

- Severity: must-change / should-change / note
- Plan risk found:
- Required plan change:
- Verification or gate to add:
- Residual risk if accepted:

---

- Revised after review: yes / no changes required
```

## Notes

- Section names are stable. Verifiers and the persona-section-map depend on
  exact section headers; do not rename them.
- Sub-bullets within a section may be added or omitted as the plan needs.
- The legacy `Active Plan Preflight Fields` style of bullet-listed fields
  (used by plans authored before this template lands) remains valid for
  pre-existing plans per the bootstrap transitional exemption; new plans
  authored after this template lands must follow the section structure
  above.
