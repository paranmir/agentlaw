# Review Method

## Purpose

Turn a draft plan into a safer revised plan by reviewing it from targeted expert
perspectives.

Personas improve the plan. They do not execute the task, replace tests, or
create an endless review loop.

## Workflow

Use this sequence when `task-classification.md` says planning is required:

1. Classify the request.
2. Draft the plan.
3. Select the persona deck.
4. Run one persona review at a time.
5. Consolidate findings.
6. Revise the plan once.
7. Execute the revised plan or ask for user approval when required.

## Persona Review Contract

Each persona review must produce plan edits, not general commentary.

Expected output per persona:

```text
Persona:
Severity: must-change / should-change / note
Plan risk found:
Required plan change:
Verification or gate to add:
Residual risk if accepted:
```

## Severity

- `must-change`: the plan should not execute until fixed.
- `should-change`: fix if cheap; execution can proceed only if the risk is
  accepted.
- `note`: useful observation, not a blocker.

## Selection Rule

1. Run the classification gate.
2. If planning is not required, do not run persona review.
3. If a plan-exempt class escalates, use the escalation deck plus the universal
   review bench.
4. If the class is conditional, use its conditional deck and, when substantial,
   the corresponding plan-required deck.
5. If a task has multiple classes, union the decks and remove duplicate roles.
6. Prefer fewer strong personas over many shallow personas.
7. Never add a persona that cannot produce a concrete plan change.

## Consolidation

After persona reviews, consolidate findings into these buckets:

- Must change before execution.
- Nice to improve if cheap.
- Accept as known residual risk.

Then revise the plan once. Avoid another review loop unless a must-change item
creates a new irreversible or high-stakes risk.

## Finding Quality Bar

A useful finding includes at least one of:

- a missed step
- a wrong sequence
- a missing verification
- a missing user gate
- a missing source or authority check
- a rollback or recovery gap
- a scope leak
- a safe simplification

Do not keep weak findings that only restate generic caution.
