# Tech Debt Tracker

## Purpose
This file tracks repeat problems, drift risks, and promotion candidates that may later need stronger repository structure or mechanical enforcement.

## How To Use
Record items here when:
- the same review comment recurs across multiple changes
- a repeated invariant should later move into lint, CI, or structural tests
- the project needs a stronger long-term guardrail than prose alone
- known debt affects future planning or correctness

## Suggested Entry Shape
For each item, record:
- title
- why it matters
- current impact
- proposed promotion target such as lint, CI, structural tests, or documentation restructure
- current status

## Review And Promotion Rules
Review tracker entries when:
- a related problem appears again
- a structural or governance change is already being made in the same area
- a repeated review rule looks stable enough for stronger enforcement

For each review, also check whether the better outcome is:
- merge overlapping entries
- simplify an overgrown entry
- close an entry because the governing text was reduced or removed rather than expanded
- add a simplification or deletion candidate, not only a new rule candidate

For each review, decide whether the item should:
- stay open as tracked debt
- be resolved through law-layer or structure updates
- be promoted into lint, CI, structural tests, or another stronger guardrail
- be closed because the risk is no longer active

Do not leave entries here indefinitely without status updates if the same area keeps changing.

## Current Entries
- No tracked items yet.
