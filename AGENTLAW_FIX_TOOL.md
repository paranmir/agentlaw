# Harness Fix Tool

## Purpose
Use this document as a practical decision tool when a current problem, harness escape, repeated mistake, or governance gap appears.

This document helps you:
- describe the problem clearly
- decide whether it is already governed
- choose the right target document
- add the smallest correct correction

It does not replace the constitution, the law layer, operational artifacts, or execution-entry documents.

## When To Use This Document
Use this document when one or more of the following is true:
- an agent bypassed the harness
- a repeated mistake appeared in review
- a current problem is visible but the owning document is unclear
- a known rule exists but seems too weak, too hidden, or too easy to bypass
- a localized law document retains generic starter placeholder content despite concrete project facts being available
- installer, memory, or continuity state appears to be bypassing file-based governance
- the MCP memory tool surface is being bypassed (raw SQL against `.harness/index/meta.db`, direct file reads where a memory tool exists, or hand-rolled walks where `agentlaw_session_restore` / `agentlaw_session_save` should be used)
- the canonical Markdown layer under `memory/*` and the index under `.harness/index/meta.db` have drifted apart
- you need to decide whether the response belongs in law, tracker, `AGENTS.md`, or enforcement

Do not use this document as a substitute for normal project work when no current governance problem exists.

## Required Inputs
Before using this document, read:
- `AGENTLAW_CONSTITUTION.md`
- the relevant file in `docs/law/*`
- `plans/tech-debt-tracker.md` when repetition or promotion may already be tracked
- `AGENTS.md` when entry routing is part of the problem

## Direct Procedure
Use the step-by-step body below as the primary procedure.
The quick flow is a map; Steps 1 through 7 are the executable sequence.

## Completion Checks
A fix-analysis run is complete only when:
- the observed problem is described with evidence
- the owning layer is explicit
- the smallest sufficient correction was chosen
- tracker and enforcement follow-up were considered
- related synchronization targets were checked when structure changed

## Failure Conditions
Treat the result as incomplete when:
- the write-up starts with a new rule name instead of the observed problem
- a higher-order rule is changed before checking lower-order corrections
- a new rule is added without checking merge, simplification, narrowing, or deletion alternatives
- repeated or mechanically detectable problems are left prose-only without a real reason

## Quick Use Flow
1. Record the problem in plain terms.
2. Classify what kind of problem it is.
3. Check whether the problem is important enough for law or tracker handling.
4. Check existing rules first.
5. Choose the owning layer.
6. Apply the smallest sufficient correction.
7. Verify that references, tracker entries, and enforcement follow-up were not missed.

## Step 1. Record The Problem
Write down:
- what happened
- what should not have happened
- what evidence supports that conclusion
- whether it seems one-off or repeated
- whether it looks local to this project or shared across multiple projects

Do not begin by inventing a rule name. Begin by describing the observed problem.

## Step 2. Classify The Problem
Classify the current problem as one or more of:
- `structure problem`
- `scope problem`
- `contract problem`
- `oracle problem`
- `failure problem`
- `entry problem`
- `execution tracking problem`
- `installer problem`
- `memory authority problem`
- `recursive promotion problem`
- `discipline problem`

Then decide which of these best describes the gap:
- `missing rule`
- `weak rule`
- `misplaced rule`
- `ignored rule`
- `mechanical gap`
- `genericization gap`

Use this step to understand the problem before deciding where to write anything.

## Step 3. Check Whether The Discrepancy Is Important
Do not assume every discrepancy belongs in the law layer.

Treat a discrepancy as important enough for law or tracker handling when one or more of the following is true:
- it changes scope, contract meaning, oracle meaning, failure interpretation, execution flow, or regression expectations
- it creates user-visible misleading behavior
- it creates maintenance risk by making false assumptions likely
- it can cause runtime breakage, invalid results, or failed flows if left implicit
- it is repeated, likely to recur, or is already a candidate for stronger enforcement

Do not treat a discrepancy as law-worthy by default when it is:
- a one-off local implementation detail with no governance impact
- obvious from code but not important to project meaning, review safety, or future maintenance
- already adequately governed by an existing law statement

## Step 4. Check Existing Rules First
Before adding a new rule, review:
- `AGENTLAW_CONSTITUTION.md`
- the relevant file in `docs/law/*`
- `AGENTS.md` if the issue concerns read-first order or short entry guardrails
- `plans/tech-debt-tracker.md` if the issue may already be tracked

The review must use **multiple synonym variants of the proposed rule's title and intent**, not a single keyword pass. A search that uses only the phrasing the agent has in mind will systematically miss rules whose original authors used different phrasing for the same intent. Concrete example: a proposed `Comment Self-Narration Prohibition` rule must also grep for `implementation history`, `changelog`, `narration`, `rename history`, `historical context`, and the specific anti-patterns the proposed rule would prohibit. Without the synonym sweep, near-duplicate § s land and the law layer drifts into self-contradiction. The full version of this obligation lives in `docs/law/REPOSITORY_ARTIFACT_RULES.md` `§New Section / Rule Addition Rule`.

Ask:
1. Does an existing rule already govern this problem?
2. Can the current rule be clarified or strengthened instead of adding a new one?
3. Is the real issue that the rule was ignored rather than missing?
4. Would a local amendment solve the problem without changing higher-order structure?
5. Can an existing rule be merged, simplified, narrowed, or deleted instead of adding another layer of text?

Default to amendment of existing text rather than creating parallel meaning.

## Step 5. Choose The Owning Layer
Use this ownership map:
- constitutional structure and invariant starter protections -> `AGENTLAW_CONSTITUTION.md`
- starter carry-forward and project-instance preservation rules -> `docs/law/STARTER_SPECIALIZATION_RULES.md`
- scope and repository boundary -> `docs/law/SCOPE.md`
- inputs, outputs, execution flow, regression expectations, readiness -> `docs/law/INPUT_OUTPUT_CONTRACT.md`
- structural and behavioral judgment -> `docs/law/ORACLE_AND_JUDGMENT.md`
- failure classes and recovery interpretation -> `docs/law/FAILURE_TAXONOMY.md`
- artifact structure, approval, synchronization, directory growth, law/artifact/entry separation -> `docs/law/REPOSITORY_ARTIFACT_RULES.md`
- installer behavior and runtime input/output expectations -> `docs/law/INPUT_OUTPUT_CONTRACT.md`
- memory authority, layout, conflict handling, and session restore behavior -> `docs/law/MEMORY_AND_CONTINUITY_RULES.md`
- MCP memory tool surface bypass and `.harness/index/meta.db` source-drift -> `docs/law/MEMORY_AND_CONTINUITY_RULES.md`
- memory authority, installer judgment, and recursive promotion judgment -> `docs/law/ORACLE_AND_JUDGMENT.md`
- installer drift, memory authority failure, and recursive promotion failure classes -> `docs/law/FAILURE_TAXONOMY.md`
- short read-first routing and short entry guardrails -> `AGENTS.md`
- repeated debt, unresolved drift, promotion candidates -> `plans/tech-debt-tracker.md`
- multi-step active corrective work -> `plans/active/*`
- durable non-authoritative supporting context -> `references/*`

Do not move a local project problem into the constitution unless it exposes a genuinely shared structural gap.

## Step 6. Apply The Smallest Sufficient Correction
Use this ladder:
1. no rule addition
2. tracker entry only
3. local law amendment
4. short `AGENTS.md` guardrail addition
5. constitutional amendment
6. enforcement follow-up under `docs/law/MECHANICAL_ENFORCEMENT_POLICY.md`

Use:
- `tracker only` when the issue is real but not stable enough for a governing rule
- `local law amendment` when the meaning clearly belongs to a law document
- `AGENTS.md` only for short entry-order corrections
- `constitutional amendment` only for shared structural gaps or starter invariants

Before choosing any rule-adding option, explicitly check whether the better correction is:
- merging overlapping rules
- simplifying an overgrown rule
- deleting stale text that no longer governs real behavior
- narrowing an over-broad rule instead of adding a second one

For existing-project bootstrap failures:
- use at least `local law amendment` when concrete repository facts are visible but law text remains generic
- use at least `tracker entry only` when important discrepancies exist but remain untracked

## Step 7. Verify Completion
After making the correction, check:
- does the new text address the actual problem
- is it placed at the correct layer
- did it preserve existing protections
- did it avoid unnecessary escalation
- did it keep `AGENTS.md` short
- did another related document also need updating
- for existing-project work, were important local facts turned into readable law
- for existing-project work, do important discrepancies remain explicitly documented in law or tracker form

If document movement, splitting, renaming, or relocation happened, also check:
- `AGENTS.md`
- `AGENTLAW_CONSTITUTION.md`
- the affected files in `docs/law/*`
- tracker or plan references

Detailed structural synchronization rules live in:
- `docs/law/REPOSITORY_ARTIFACT_RULES.md`

## When To Escalate To Enforcement
If the same problem keeps recurring and is mechanically detectable, do not keep solving it only with prose.

Use:
- `docs/law/MECHANICAL_ENFORCEMENT_POLICY.md`

Especially when:
- the problem is repeated
- the cost of failure is meaningful
- the failure can be detected through lint, CI, structural checks, tests, or equivalent mechanisms

## Recommended Output Shape
When writing up the result of this process, make these explicit:
- `problem`
- `evidence`
- `analysis`
- `repetition status`
- `existing rule check result`
- `merge, simplify, narrow, or delete alternative check result`
- `owning layer`
- `chosen target`
- `whether tracker follow-up is needed`
- `whether enforcement follow-up is needed`

## Next Update Trigger
Update this document when:
- the decision flow proves unclear in real use
- rule-placement mistakes keep recurring
- the tool stops helping users choose the right layer quickly
