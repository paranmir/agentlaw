# Core Persona Deck

## Purpose

Universal review bench. Apply to every non-trivial plan-required task per
`docs/law/PLANNING_AND_REVIEW_RULES.md` § Required Planning Workflow. The
universal personas evaluate the plan's foundational dimensions independently
of which substance domains the plan touches.

For trivial plan-required work, only the Trigger Coverage Verifier persona
runs.

## Sequential Application Contract

Each universal persona runs in **its own review turn**. Lens injection from
one persona must not cross-contaminate the next persona's review. This
preserves persona-specific output qualities that are sensitive to prompt
priming.

The **Trigger Coverage Verifier always runs first**. Its job is to validate
that the plan's classification (trivial / non-trivial) and Domain Coverage
self-mark match the plan's actual content. If it finds a missed trigger or
missing domain mark, the workflow restarts from plan revision.

After Trigger Coverage Verifier passes, the remaining universal personas
run in any order; the order does not matter as long as each persona's
review is recorded separately.

## Universal Personas (24)

The Trigger Coverage Verifier always runs first. The remaining personas are
grouped by domain for navigation; **within and across domain groups, order
of execution does not matter** (each persona runs in its own turn — see
§ Sequential Application Contract above).

### Classification Gatekeeper (runs first)

| Persona | Plan-improvement mandate |
| --- | --- |
| **Trigger Coverage Verifier** | Read the plan and validate that (a) cited risk triggers from `task-classification.md` § Non-Trivial Triggers actually match plan content, (b) the trivial / non-trivial classification is correct, and (c) for non-trivial plans, the Domain Coverage self-mark covers all conditional domains the plan content touches. Flag missing or wrong markings; recommend reclassification. |

### Domain 1 — Intent (4)

| Persona | Plan-improvement mandate |
| --- | --- |
| **Intent Statement Reviewer** | Verify the plan opens with a restatement of the user's ask in the author's own words, in a form the user can confirm. Reject vague restatements. |
| **Underlying-Need Reviewer** | Verify the plan distinguishes the literal ask from the user's underlying need. Reject platitudes ("improve productivity") in the underlying-need field; require a specific outcome. |
| **Artifact Shape Reviewer** | Verify the plan names the deliverable's specific form (format, length, structure). Reject vague shape descriptions. |
| **Acceptance Criteria Reviewer** | Verify the plan names concrete criteria the user (or a reasonable tool) can check. Reject abstract phrasings like "works well" or "no problems". |

### Domain 2 — Scope (5)

| Persona | Plan-improvement mandate |
| --- | --- |
| **Affected Surface Reviewer** | Verify the plan enumerates every type of surface it touches (work artifacts, related external systems and people, durable records, environmental and contextual assumptions). Reject single-category lists. |
| **Stakeholder Map Reviewer** | Verify stakeholders are segmented by role and interest in a way meaningful to the plan's context. Reject single generic labels like "users". |
| **Downstream Impact Reviewer** | Verify the plan presents evidence (search/query results, dependency listings) of downstream consumers. Reject claims without evidence ("none affected"). |
| **Indirect Effect Reviewer** | Verify the plan enumerates second-order effects across multiple categories (related artifact updates, stored state, alerts, environmental settings, user expectations). Reject single-category enumeration. |
| **Temporal Scope Reviewer** | Verify the plan declares its temporal scope (one-time, ongoing, time-bounded) and the operational meaning of that choice. |

### Domain 3 — Premise (universal subset, 4)

| Persona | Plan-improvement mandate |
| --- | --- |
| **Premise & Source Reviewer** | For each factual claim, verify the plan cites (a) a verification artifact reference (observation, source ID, system response, measurement), (b) a specific source identifier, and (c) a freshness threshold (when the source becomes stale). All three together. Reject method-name-only references. |
| **Assumption-Path Reviewer** | For each assumption, verify it is labeled (assumed / unknown / TBD), the basis is cited, and a path to verification is named (what evidence/event would convert the assumption into a verified fact). |
| **Prerequisite Chain Reviewer** | Verify the plan presents an explicit prerequisite tree (or graph), with each node verified. |
| **Prior Art Reviewer** | Verify the plan presents a search trace (queries used, locations checked) for prior solutions or reusable artifacts. Reject "checked, none" without trace. |

### Domain 4 — Risk (4)

| Persona | Plan-improvement mandate |
| --- | --- |
| **Risk Classification Reviewer** | Verify each step is classified by reversibility (reversible / hard-to-reverse / irreversible) and blast radius is named. |
| **User Gate Reviewer** | Verify user gates are explicit before each non-reversible step; specify when and what the user is being asked to approve. For irreversible steps, require explicit irreversibility acknowledgment. |
| **Rollback Path Reviewer** | Verify each non-reversible step names a rollback or recovery path. |
| **Fallback Plan Reviewer** | Verify each critical assumption has a fallback for failure. Require cascading fallback (what happens if the first fallback also fails). |

### Domain 5 — Verification (1)

| Persona | Plan-improvement mandate |
| --- | --- |
| **Verification Reviewer** | Verify the plan names pre-action and post-action checks at execution boundaries, and a verification strategy (tests, oracles, controlled checks) matched to risk level, including negative or adversarial cases for new mechanisms. Reject normal-case-only strategies. |

### Domain 12 — Continuity (universal subset, 1)

| Persona | Plan-improvement mandate |
| --- | --- |
| **Internal Continuity Reviewer** | Verify the plan provides handoff state (what the next session or worker needs to resume) and a decision log (decisions made plus their rationale). |

### Domain 13 — Governance (1)

| Persona | Plan-improvement mandate |
| --- | --- |
| **Governance & Compliance Reviewer** | Verify the plan checks consistency among the rules, policies, and agreements it cites or applies. Where multiple rules apply, require explicit cross-rule consistency check and conflict-resolution decision. |

### Domain 15 — Confidence (universal subset, 2)

| Persona | Plan-improvement mandate |
| --- | --- |
| **Confidence Label Reviewer** | Verify each non-trivial claim is labeled with confidence (verified / assumed / unknown / TBD), the basis for the label is cited, and (for non-verified labels) a verification path is named. |
| **Undecided Item Reviewer** | Verify the plan lists undecided / TBD items explicitly, names unblocking conditions for each, and assigns a decision owner. |

### Domain 16 — Self-Application (universal subset, 1)

| Persona | Plan-improvement mandate |
| --- | --- |
| **Rule-on-Self Reviewer** | When the plan introduces or modifies a rule, verify the plan's body itself follows the rule (or invokes explicit bootstrap exemption per `docs/law/PLANNING_AND_REVIEW_RULES.md` § Bootstrap Transitional Exemption). For plans that do not modify rules, this persona PASSes with reason "plan does not modify rules". |

## Minimum Use

- **Trivial plan-required work**: only Trigger Coverage Verifier runs. The
  remaining 23 universal personas are skipped.
- **Non-trivial plan-required work**: all 24 universal personas run, plus
  any substance-triggered personas marked applicable in the plan's Domain
  Coverage section. Per `persona-decks-specialized.md`.

Never add a persona that cannot produce a concrete plan change. A persona
that always returns "no findings" for a given class of plan should be
marked as "PASS — no applicable concern" rather than skipped silently.

## Section Mapping

Each universal persona's primary and optional plan-template sections are
recorded in `docs/planning-protocol/persona-section-map.md`. When a plan
revision affects only specific sections, only the personas mapped to those
sections need re-invocation.

## Cross-References

- Law authority: `docs/law/PLANNING_AND_REVIEW_RULES.md`
- Triggers and classification: `docs/planning-protocol/task-classification.md`
- Workflow and severity: `docs/planning-protocol/review-method.md`
- Substance-triggered and field-specific personas:
  `docs/planning-protocol/persona-decks-specialized.md`
- Section mapping: `docs/planning-protocol/persona-section-map.md`
- Plan template: `docs/planning-protocol/plan-template.md`
