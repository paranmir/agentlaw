# Persona Section Map

## Purpose

Map each review persona to the plan-template sections it primarily reviews,
plus optional sections it may consult. The map drives **section-based
revision re-review** per `docs/law/PLANNING_AND_REVIEW_RULES.md` § Required
Planning Workflow: when a plan is revised after review, only the personas
whose mapped sections were revised are re-invoked.

## Format

- **Tier**: `universal` (always evaluated for non-trivial work),
  `substance-triggered` (evaluated when the corresponding Domain Coverage
  box is marked), or `sensitive-domain` (evaluated when the task class is
  in the sensitive-domain family).
- **Primary sections**: the persona must read these to do its review.
- **Optional sections**: the persona may consult these for context but
  does not need them to produce a finding.

When section-based re-review applies, a persona is re-invoked if **any of
its primary sections** changed in the revision. Optional-only changes do
not trigger re-invocation.

## Universal Personas

| Persona | Tier | Primary sections | Optional sections |
| --- | --- | --- | --- |
| Trigger Coverage Verifier | universal | Classification, Domain Coverage | (whole plan body) |
| Intent Statement Reviewer | universal | Intent | Status |
| Underlying-Need Reviewer | universal | Intent | Scope |
| Artifact Shape Reviewer | universal | Intent | Verification |
| Acceptance Criteria Reviewer | universal | Intent | Verification, Risk |
| Affected Surface Reviewer | universal | Scope | Premise, Risk |
| Stakeholder Map Reviewer | universal | Scope | Continuity |
| Downstream Impact Reviewer | universal | Scope | External Contract & Compatibility, Long-term Care |
| Indirect Effect Reviewer | universal | Scope | Risk, Long-term Care |
| Temporal Scope Reviewer | universal | Scope | Time & Sequencing |
| Premise & Source Reviewer | universal | Premise | Confidence |
| Assumption-Path Reviewer | universal | Premise, Confidence | Risk |
| Prerequisite Chain Reviewer | universal | Premise | Risk, Verification |
| Prior Art Reviewer | universal | Premise | Scope |
| Risk Classification Reviewer | universal | Risk | Scope, Verification |
| User Gate Reviewer | universal | Risk | Status, Continuity |
| Rollback Path Reviewer | universal | Risk | Verification |
| Fallback Plan Reviewer | universal | Risk | Premise, Confidence |
| Verification Reviewer | universal | Verification | Risk, Acceptance Criteria |
| Internal Continuity Reviewer | universal | Continuity | Status |
| Governance & Compliance Reviewer | universal | Governance | Premise |
| Confidence Label Reviewer | universal | Confidence | Premise |
| Undecided Item Reviewer | universal | Confidence | Continuity |
| Rule-on-Self Reviewer | universal | Self-Application | Classification, Governance |

## Substance-Triggered Personas

| Persona | Tier | Primary sections | Optional sections |
| --- | --- | --- | --- |
| Source Conflict Reviewer | substance-triggered (Premise) | Premise | Confidence |
| Runtime State Reviewer | substance-triggered (Premise) | Premise | Verification |
| Access-Premise Reviewer | substance-triggered (Premise) | Premise | Security & Trust Boundaries |
| Trust Boundary Reviewer | substance-triggered (Security) | Security & Trust Boundaries | Scope, Data Stewardship |
| Sensitive Information Handling Reviewer | substance-triggered (Security) | Security & Trust Boundaries | Data Stewardship |
| Permission Boundary Reviewer | substance-triggered (Security) | Security & Trust Boundaries | Premise (Access-Premise) |
| Adversarial Path Reviewer | substance-triggered (Security) | Security & Trust Boundaries | Risk, Verification |
| Supply Chain Reviewer | substance-triggered (Security) | Security & Trust Boundaries | Premise |
| Information Integrity Reviewer | substance-triggered (Data) | Data Stewardship | Premise, Verification |
| Privacy & Retention Reviewer | substance-triggered (Data) | Data Stewardship | Security & Trust Boundaries |
| Interface Contract Reviewer | substance-triggered (Contract) | External Contract & Compatibility | Scope, Long-term Care |
| Sequencing & Parallel Execution Reviewer | substance-triggered (Time) | Time & Sequencing | Risk, Verification |
| Deadline Reviewer | substance-triggered (Time) | Time & Sequencing | Risk |
| Cost & Efficiency Reviewer | substance-triggered (Cost) | Cost, Resource & Performance | Scope, Long-term Care |
| Long-term Care Reviewer | substance-triggered (Long-term) | Long-term Care | Risk, Continuity |
| Documentation Impact Reviewer | substance-triggered (Long-term) | Long-term Care | Scope, Continuity |
| Persistent Record Reviewer | substance-triggered (Continuity) | Continuity | Scope, Governance |
| Bounded Knowledge Reviewer | substance-triggered (Confidence) | Confidence | Risk, Premise |
| Confidence-Shift Reviewer | substance-triggered (Confidence) | Confidence | Risk, Verification |
| Coherent Example Reviewer | substance-triggered (Self-Application) | Self-Application | Governance |
| Rule Lifecycle Reviewer | substance-triggered (Self-Application) | Self-Application | Bootstrap Transitional Declaration, Governance |

## Sensitive-Domain Field Personas

These personas apply only within sensitive-domain task classes (per
`task-classification.md`) and use field-specific vocabulary. They review
deliverable-quality dimensions that do not generalize to arbitrary
domains.

| Persona | Tier | Primary sections | Optional sections |
| --- | --- | --- | --- |
| Voice and Tone Reviewer | sensitive-domain (writing/editing) | Intent (artifact shape), Acceptance Criteria | Scope (audience) |
| Risk Language Reviewer | sensitive-domain (writing/editing/legal/medical) | Intent (artifact shape), Acceptance Criteria | Governance, Risk |
| Locale Specialist | sensitive-domain (translation/localization) | Intent (artifact shape) | Premise (sources for terminology) |
| Meaning Fidelity Reviewer | sensitive-domain (translation/localization) | Intent | Premise |
| Cultural Risk Reviewer | sensitive-domain (translation/localization, creative) | Intent, Acceptance Criteria | Scope (stakeholders) |
| Sensitive Language Reviewer | sensitive-domain (sensitive translation) | Intent | Premise |
| Risk and Sensitivity Reviewer | sensitive-domain (creative) | Intent, Acceptance Criteria | Risk |
| Accessibility Reviewer | sensitive-domain (visual/UI) | Intent (artifact shape), Acceptance Criteria | Scope (stakeholders) |
| Autonomy Preserver | sensitive-domain (personal advice/coaching) | Intent, Acceptance Criteria | Continuity |
| Emotional Reality Reviewer | sensitive-domain (personal advice/coaching) | Intent | Acceptance Criteria |
| Support Network Reviewer | sensitive-domain (personal advice/coaching) | Intent (acceptance/escalation) | Continuity |
| Learner Level Reviewer | sensitive-domain (explanation/teaching) | Intent, Acceptance Criteria | Scope (stakeholders) |

## Section Naming

The section names above match `docs/planning-protocol/plan-template.md`
exactly. Renaming a section in the template requires updating this map and
republishing both files together (Stage 4 mirror obligation).

## Section-Based Re-Review — Worked Example

Suppose a non-trivial plan is reviewed and the consolidation step requires
revisions to:

- the **Premise** section (a fact's verification was challenged), and
- the **Risk** section (a fallback was added).

Re-review invokes only personas whose primary sections include Premise or
Risk:

- Premise: Premise & Source Reviewer, Assumption-Path Reviewer, Prerequisite
  Chain Reviewer, Prior Art Reviewer, Source Conflict Reviewer (if marked),
  Runtime State Reviewer (if marked), Access-Premise Reviewer (if marked).
- Risk: Risk Classification Reviewer, User Gate Reviewer, Rollback Path
  Reviewer, Fallback Plan Reviewer.

Personas mapped to other primary sections (e.g., Intent Statement Reviewer)
are not re-invoked. Their prior findings stand because their sections did
not change.

The Trigger Coverage Verifier always re-runs after revisions because its
primary sections (Classification, Domain Coverage) implicitly depend on the
plan body and may need to re-check that revisions did not introduce a new
trigger.

## Cross-References

- Law authority: `docs/law/PLANNING_AND_REVIEW_RULES.md`
- Plan template: `docs/planning-protocol/plan-template.md`
- Workflow: `docs/planning-protocol/review-method.md`
- Persona decks: `docs/planning-protocol/persona-decks-core.md`,
  `docs/planning-protocol/persona-decks-specialized.md`
