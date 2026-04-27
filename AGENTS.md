# Repository Guidelines

## Purpose
This document is a short execution-entry map for a project using the Harness kit. The source of truth is not `AGENTS.md`, but the constitution-root-control-law-operational-artifact-execution-entry system.

Any coding agent working in this repository must follow the read-first order and governing hierarchy below.

## What This Repository Is
This repository starts from the Harness kit.

- Purpose of `AGENTS.md`: act as a short execution-entry map for the current project
- Purpose of the law layer: store the real working criteria under `docs/law/*`
- Purpose of `AGENTLAW_INIT_TOOL.md`: provide a shared bootstrap entry point for empty, near-empty, or already-existing target repositories
- Purpose of the root control layer: provide starter-level bootstrap and corrective-governance guidance before detailed law work

## Source of Truth
Document priority is:

1. `AGENTLAW_CONSTITUTION.md`
2. root control documents
3. `docs/law/*`
4. approved structured repository artifacts such as `plans/*`, `references/*`, and stable generated facts when they exist
5. approved continuity or memory records
6. `AGENTS.md` and other execution-entry documents

Important:
- Project-specific rules belong in `docs/law/*`.
- Continuity or memory records are derived context below file-based governance.
- `AGENTS.md` is not a rule store. It only tells agents what to read first and how to enter the work.
- `RULES.md` is not used.

## Read First
Check the current agreement in this order:

1. `AGENTLAW_CONSTITUTION.md`
2. Root control tools, in this order when relevant:
   - `AGENTLAW_INIT_TOOL.md` for initialization or rebuild
   - `AGENTLAW_UPDATE_TOOL.md` for incorporating upstream harness changes into an existing project
   - `AGENTLAW_FIX_TOOL.md` for current problems, harness escape, or repeated rule failure
3. Law layer, in this order when relevant:
   - `docs/law/SCOPE.md`
   - `docs/law/INPUT_OUTPUT_CONTRACT.md`
   - `docs/law/ORACLE_AND_JUDGMENT.md`
   - `docs/law/CODE_AUTHORSHIP_AND_STEWARDSHIP_RULES.md`
   - `docs/law/FAILURE_TAXONOMY.md`
   - `docs/law/REPOSITORY_ARTIFACT_RULES.md`
   - `docs/law/MEMORY_AND_CONTINUITY_RULES.md`
   - `docs/law/STARTER_SPECIALIZATION_RULES.md`
   - `docs/law/MECHANICAL_ENFORCEMENT_POLICY.md`
4. `AGENTS.md`

If the project grows in complexity, also read these when they exist and are relevant:
- `memory/working-set.md`
- `memory/LOOKUP_RULES.md`
- `plans/active/*`
- `plans/completed/*`
- `plans/tech-debt-tracker.md`
- `references/*`

Prefer them in this order:
1. `memory/working-set.md` and `memory/LOOKUP_RULES.md` for session continuity
2. `plans/active/*` for current execution-driving work
3. `references/*` for repository-local reference context
4. `plans/tech-debt-tracker.md` for repeated drift or promotion candidates
4. `plans/completed/*` only when historical decisions or finished work need to be consulted

Create or update them when:
- a multi-step change is too large or long-lived to track safely in `AGENTS.md`
- a decision path needs durable progress tracking across sessions
- reference material would otherwise need to be re-derived or re-read repeatedly
- repeated drift or review failures need tracking for later promotion into stronger enforcement

Move a plan from `plans/active/*` to `plans/completed/*` when:
- the governed work is complete for its current scope
- the main decisions and outcome are recorded clearly enough for later review
- the plan no longer needs to drive current execution

## Current Priority
The current core work order is fixed:

1. Clarify the law layer before implementation
2. Keep `AGENTS.md` aligned as a short execution entry map only
3. Make unresolved items explicit instead of hiding them
4. Update `docs/law/*` when project meaning changes

## Working Rules
This file is a routing map only. Governing rules live in the constitution and `docs/law/*`. Read those before acting.
