# Specialized Persona Decks

## Purpose

Specialized personas review the plan from substance-specific lenses. They
are invoked **only when the relevant substance is present in the plan**:

- **Substance-triggered personas**: invoked when the plan's Domain Coverage
  self-mark (in `plan-template.md`) marks the corresponding domain as
  applicable. The Trigger Coverage Verifier (universal) validates the
  self-mark; missing marks trigger reclassification.
- **Sensitive-domain field personas**: invoked when the plan's task class is
  in the sensitive-domain family (writing/translation/personal-advice/
  legal-financial-medical/etc.). Field-specific by design; do not absorb
  into structural domains.

All specialized personas run **sequentially in their own review turns** per
`review-method.md` § Workflow.

## Substance-Triggered Personas (21)

### Domain 3 (Premise) — substance subset

| Persona | Trigger | Plan-improvement mandate |
| --- | --- | --- |
| **Source Conflict Reviewer** | plan cites multiple sources that disagree on a load-bearing claim | Require an explicit conflict-resolution rule (most recent / most authoritative / user-decision-gate). Reject plans that pick a side silently. |
| **Runtime State Reviewer** | plan depends on the actual state of a system, environment, or context being a specific way at execution time | Require an active probe at execution time to confirm the assumed state. Reject static claims about runtime state without verification step. |
| **Access-Premise Reviewer** | plan claims to have a permission or access right that must hold for execution to succeed | Require evidence that the access is actually held (test invocation result, authentication verification, written grant). Reject claim-only assertions. |

### Domain 6 (Security & Trust Boundaries) — 5

| Persona | Trigger | Plan-improvement mandate |
| --- | --- | --- |
| **Trust Boundary Reviewer** | plan modifies how data or control flows between regions of different trust levels | Identify each crossing point and require validation, filtering, and audit-trail mechanisms at the boundary. |
| **Sensitive Information Handling Reviewer** | plan handles sensitive identifiers (credentials, secrets, identity tokens, personal data, privileged communications, private content) | Trace the lifecycle of each sensitive value; flag plaintext storage, inappropriate exposure, and over-broad delegation. |
| **Permission Boundary Reviewer** | plan grants, scopes, or revokes permissions or authorization | Verify the granted authority is the minimum required. Reject over-broad, perpetual, or unscoped grants. |
| **Adversarial Path Reviewer** | plan introduces a surface that could be exercised by malicious or accidental misuse | Enumerate adversarial scenarios (system-side threat model + user-side abuse cases). Require concrete plan changes for each non-trivial scenario. |
| **Supply Chain Reviewer** | plan introduces or upgrades external dependencies | Verify provenance, integrity, and trust assumptions of each new external dependency. |

### Domain 7 (Data Stewardship) — 2

| Persona | Trigger | Plan-improvement mandate |
| --- | --- | --- |
| **Information Integrity Reviewer** | plan transforms, moves, or transitions persistent information | Verify meaning, relationships, and completeness are preserved across the transformation. Detect and recover from data drift over time (information that loses validity as external context evolves). |
| **Privacy & Retention Reviewer** | plan handles personally identifiable or sensitive information | Apply minimum-collection, purpose-limited-use, and retention-bounded policies. Flag violations. |

### Domain 8 (External Contract & Compatibility) — 1

| Persona | Trigger | Plan-improvement mandate |
| --- | --- | --- |
| **Interface Contract Reviewer** | plan modifies an externally exposed surface (API, CLI, file format, protocol, or content meant for use outside the producing context) | Verify backward compatibility for active consumers; require versioning strategy and deprecation/migration path for removed surfaces; for distributed content (used or copied in other codebases / projects / teams), verify the content is sufficiently abstracted from local specifics (vocabulary, paths, examples, environmental assumptions) to function across the intended consumer range. |

### Domain 9 (Time & Sequencing) — 2

| Persona | Trigger | Plan-improvement mandate |
| --- | --- | --- |
| **Sequencing & Parallel Execution Reviewer** | plan has multiple steps with order dependency or possible parallel execution | Enumerate ordering constraints and concurrency conflicts (resource contention, ordering-assumption violations, duplicate effects). |
| **Deadline Reviewer** | plan has time-bound elements (deadline, expiry, freshness window) | Verify the time constraint is named, the consequence of missing it is stated, and a path to handle missed deadlines exists. |

### Domain 10 (Cost, Resource & Performance) — 1

| Persona | Trigger | Plan-improvement mandate |
| --- | --- | --- |
| **Cost & Efficiency Reviewer** | plan consumes meaningful resources (time, money, compute, storage, attention) | Estimate resource consumption and impact on execution efficiency or responsiveness. Reject "minor impact" without backing. |

### Domain 11 (Long-term Care) — 2

| Persona | Trigger | Plan-improvement mandate |
| --- | --- | --- |
| **Long-term Care Reviewer** | plan produces an artifact that requires ongoing care (maintenance, updates, monitoring) | Estimate future maintenance and modification cost; assess continued operability and observability (failure detection, status visibility, operator response paths). Make accumulated technical/operational debt explicit. |
| **Documentation Impact Reviewer** | plan changes something that other people, teams, or future agents read about | Verify related documentation accurately reflects the change — not whether docs were updated, but whether the artifact and the descriptive material remain consistent. |

### Domain 12 (Continuity) — substance subset

| Persona | Trigger | Plan-improvement mandate |
| --- | --- | --- |
| **Persistent Record Reviewer** | plan's outcome must be reflected in durable records outside the plan body itself (memory, registry, configuration, logbook, contracts) | Identify which durable records require updating; ensure each is named in the plan and has a verification check. |

### Domain 15 (Confidence) — substance subset

| Persona | Trigger | Plan-improvement mandate |
| --- | --- | --- |
| **Bounded Knowledge Reviewer** | plan operates near the edge of known information; surprises are plausible | Require an enumeration of concrete scenarios that would invalidate the plan's premises. Reject generic "edge cases may surprise us" without concrete scenarios. |
| **Confidence-Shift Reviewer** | plan executes over time during which confidence in some claim may shift | Require an escalation protocol naming the threshold at which confidence drop triggers pause, reconsultation, or replan. |

### Domain 16 (Self-Application) — substance subset

| Persona | Trigger | Plan-improvement mandate |
| --- | --- | --- |
| **Coherent Example Reviewer** | plan introduces or modifies a rule and the rule body includes examples (exemplars, anti-examples, illustrations) | Verify each example self-consistently demonstrates the rule. Reject examples that themselves violate the rule's substance. |
| **Rule Lifecycle Reviewer** | plan introduces or modifies a rule, especially when the rule constrains future plans (including its own future revisions) | Verify (a) bootstrap transitional exemption is explicitly declared if self-application is impossible, (b) handling for pre-existing artifacts under the old rule is named (retroactive / prospective / case-by-case), (c) the rule allows itself to be revised by future plans without infinite regress or absurd constraint, with the revision protocol named. |

## Sensitive-Domain Field Personas (~12)

These personas apply only within sensitive-domain task classes (writing,
translation, personal advice, legal/financial/medical, accessibility-
critical visual work, etc.). They use field-specific vocabulary by design.
Their substance does not generalize to arbitrary domains, and they are not
absorbed into the structural 16-domain system. Invoke them when the plan's
task class is in the relevant family.

### Substantive Writing & Editing

| Persona | Mandate |
| --- | --- |
| **Voice and Tone Reviewer** | Verify the deliverable's voice and tone match the audience and purpose. Distinct from generic "Acceptance Criteria"; tone has aesthetic and brand dimensions that aren't checkbox-verifiable. |
| **Risk Language Reviewer** | Verify the deliverable's wording does not create legal, reputational, or interpersonal risk inadvertently (overpromising, defamation, condescension, exclusion). |

### Translation & Localization

| Persona | Mandate |
| --- | --- |
| **Locale Specialist** | Verify locale-specific conventions (date format, currency, address structure, formality register) are honored. |
| **Meaning Fidelity Reviewer** | Verify the translation preserves source meaning, not only literal word equivalence. |
| **Cultural Risk Reviewer** | Verify the translation does not introduce cross-cultural offense, taboo violations, or misread idioms. |
| **Sensitive Language Reviewer** | For translation of sensitive content (legal, medical, mental-health, identity), verify the chosen terms in the target locale carry the intended weight without unintended harm. |

### Creative & Visual Work

| Persona | Mandate |
| --- | --- |
| **Risk and Sensitivity Reviewer** | For creative work that could touch sensitive territory (identity, trauma, controversy), verify direction has explicit sensitivity boundaries and review checkpoints. |
| **Accessibility Reviewer** | For visual or interactive work, verify accessibility commitments (alt text, contrast, keyboard navigation, screen-reader compatibility, cognitive load). |

### Personal Advice & Coaching (Sensitive)

| Persona | Mandate |
| --- | --- |
| **Autonomy Preserver** | Verify advice respects the recipient's autonomy: present options rather than directives, name uncertainty, avoid manipulation. |
| **Emotional Reality Reviewer** | Verify advice acknowledges the emotional reality of the situation (validation before reframing) and does not dismiss feelings to reach a conclusion faster. |
| **Support Network Reviewer** | Verify advice considers the recipient's network of care (professionals, family, peers) and identifies escalation paths to qualified humans when relevant. |

### Explanation & Teaching

| Persona | Mandate |
| --- | --- |
| **Learner Level Reviewer** | Verify the explanation matches the learner's current level — neither over-explaining (insulting) nor under-explaining (lost). |

## Selection Rule

For non-trivial plan-required work, after the universal personas in
`persona-decks-core.md` run:

1. For each conditional domain marked applicable in the plan's Domain
   Coverage self-mark, run the corresponding substance-triggered personas
   sequentially.
2. For each sensitive-domain task class in the plan's classification, run
   the corresponding sensitive-domain field personas sequentially.
3. Each persona runs in its own review turn (lens-injection isolation).
4. Within domain, persona order is not significant.

When a plan has multiple task classes that draw from overlapping field
personas (e.g., a translation of a personal-advice piece that touches
substantive writing), run each persona only once.

Never add a persona that cannot produce a concrete plan change.

## Section Mapping

Each specialized persona's primary and optional plan-template sections are
recorded in `docs/planning-protocol/persona-section-map.md`.

## Cross-References

- Law authority: `docs/law/PLANNING_AND_REVIEW_RULES.md`
- Triggers and classification: `docs/planning-protocol/task-classification.md`
- Workflow and severity: `docs/planning-protocol/review-method.md`
- Universal personas: `docs/planning-protocol/persona-decks-core.md`
- Section mapping: `docs/planning-protocol/persona-section-map.md`
- Plan template: `docs/planning-protocol/plan-template.md`
- Migration: `docs/references/deck-absorption-matrix-2026-04-29.md`
