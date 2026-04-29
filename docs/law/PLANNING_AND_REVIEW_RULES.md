# Planning And Review Rules

## Purpose

This document defines when agents must plan, how draft plans must be reviewed,
and where the operational planning protocol lives.

The law layer owns the obligation. The operational details live in
`docs/planning-protocol/*` so planning rules stay usable without bloating
`docs/law/*`.

## Authority

This document governs planning behavior for all non-trivial agent work, not only
code work.

When this document and a protocol document appear to conflict, this document
wins. Protocol documents may clarify classification, review method, and persona
selection, but they must not weaken the obligations here.

## Planning Gate

Before meaningful work begins, the agent must classify the user's request.

The agent may skip a repository-tracked plan only when the request is clearly
low-risk, answer-only, short, non-current, non-mutating, and does not involve a
high-stakes domain or durable process change.

The agent must create or extend a plan when the request is multi-step,
state-changing, high-risk, externally consequential, freshness-sensitive,
governance-affecting, release/deploy oriented, or likely to require verification
through tests, citations, calculations, previews, dry-runs, or other evidence.

## Required Planning Workflow

When planning is required, the agent must follow this workflow before executing
the plan:

1. Classify the request.
2. Draft the plan.
3. Review the draft with the applicable persona deck.
4. Consolidate the review findings.
5. Produce the revised plan.

The revised plan is the execution plan. Persona review comments are not the goal;
they are inputs for improving the plan.

Plan documents must not preserve long duplicate "draft plan" and "revised plan"
bodies by default. That duplicates content and makes plans harder to maintain.
Instead, review-required plans must record compact review evidence fields:

- `Review required`
- `Plan reviewed`
- `Personas applied`
- `Revised after review`

When review is not required, the plan must record `Review exemption reason`.

Review-required plans must also include a short `Separate Persona Review
Passes` section. Each persona pass must record concrete findings using the
review-method fields: severity, plan risk found, required plan change,
verification or gate to add, and residual risk if accepted.

The plan may include short review notes when useful, but the evidence fields and
the separate persona-pass section are the parseable contract that proves the
plan was not treated as ready before review. An agent must not mark `Plan
reviewed: yes` before those persona passes are actually performed and recorded.

If the agent discovers during implementation that review was skipped, compressed
into a retrospective claim, or falsely marked complete, implementation must stop.
The agent must return to plan correction, record the failure when it is
governance-relevant, re-run the applicable persona review, and only then resume
execution from the revised plan.

## Operational Sources

The canonical operational planning sources are:

- `docs/planning-protocol/task-classification.md` for deciding whether planning
  is required and which task class applies
- `docs/planning-protocol/review-method.md` for the plan review and
  consolidation procedure
- `docs/planning-protocol/persona-decks-core.md` for the universal review bench
- `docs/planning-protocol/persona-decks-specialized.md` for class-specific
  review personas

Agents should read only the protocol sections needed for the current task class
unless ambiguity or risk requires broader review.

## Runtime Reminder Contract

Runtime reminder packets should point agents at the planning protocol when they
surface plan discipline guidance.

The `plan_discipline_reminder` returned by `agentlaw_session_restore` and
`agentlaw_session_save` must include these operational pointers:

- `classification_source`: `docs/planning-protocol/task-classification.md`
- `review_method_source`: `docs/planning-protocol/review-method.md`
- `persona_deck_sources`: `docs/planning-protocol/persona-decks-core.md` and
  `docs/planning-protocol/persona-decks-specialized.md`
- `review_evidence_fields`: the compact field names that make a plan ready
  plus the `Separate Persona Review Passes` evidence section name

Runtime surfaces the reminder; the agent remains responsible for judging
whether planning is required and applying the workflow.

## Review Discipline

Persona review must produce concrete plan improvements.

Useful findings identify a missing step, wrong sequence, missing verification,
missing user gate, missing source, rollback gap, scope leak, or safe
simplification.

Weak findings that only restate generic caution should not survive
consolidation.

## Trivial Work Boundary

This rule must not turn every response into a ceremony.

For plan-exempt work, the agent may answer directly after a lightweight internal
check. If a task starts plan-exempt but gains file edits, external effects,
current-source dependence, high-stakes consequences, or durable process impact,
it escalates into the planning workflow.

## Completion And Handoff

For repository-tracked work, the plan must name affected surfaces, verification,
rollback or recovery, non-goals, and any new law or reference artifacts.

When the work completes, move the active plan to the completed plan directory
with enough context for a later agent to reconstruct the intended behavior,
review decisions, verification, and residual risks.
