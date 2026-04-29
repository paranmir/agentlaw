# Planning Protocol

## Purpose

This directory contains the operational protocol agents use when a request needs
a plan.

Law owns the obligation in `docs/law/PLANNING_AND_REVIEW_RULES.md`. This
directory owns the reusable classification, review method, and persona deck
details.

## Use Order

1. Read `task-classification.md` to decide whether planning is required.
2. If planning is required, draft the plan.
3. Read `review-method.md` to run the review and consolidation loop.
4. Apply `persona-decks-core.md`.
5. Add only the relevant sections from `persona-decks-specialized.md`.
6. Revise the plan once from consolidated findings.

## File Map

| File | Use |
| --- | --- |
| `task-classification.md` | Classify the request and decide whether planning is required. |
| `review-method.md` | Run the draft-plan review, finding consolidation, and revised-plan step. |
| `persona-decks-core.md` | Apply the universal review bench for every plan-required task. |
| `persona-decks-specialized.md` | Select class-specific reviewers for escalated, conditional, or plan-required tasks. |

## Reading Budget

Do not bulk-read this directory for trivial tasks.

For plan-required work, read the classification file, review method, the core
deck, and only the specialized sections matching the task class. For multi-class
work, union the relevant personas and remove duplicates.

## Output Contract

The final plan should make these visible:

- task class and planning reason
- affected surfaces
- public or external contract impact
- verification or evidence oracle
- user gates for irreversible, paid, public, destructive, or account-changing steps
- rollback, repair, or recovery path
- explicit non-goals
- residual risks accepted after persona review
