# Problem Analysis And Rule Addition

## Purpose
This document defines how a coding agent should respond when work reveals a current problem, repeated mistake, harness escape, or governance gap.

Its role is not to replace the constitution, law layer, operational artifacts, or execution-entry documents.

Its role is to:
- analyze the current problem before reacting to it
- determine whether existing governing documents already cover it
- decide the owning layer for any corrective rule
- add the smallest correct rule at the correct layer
- prevent silent deletion, over-promotion, and rule sprawl

## When To Use This Document
Use this document when one or more of the following is true:
- an agent made a mistake that bypassed or weakened the harness
- the repository exposed a repeated review failure
- a current problem is visible but the owning rule is unclear
- a known rule existed but was too weak, too hidden, or too easy to bypass
- the same issue may need promotion into stronger enforcement later

Do not use this document as a substitute for ordinary project work when no current problem or rule gap exists.

## Problem Intake
Before adding or changing any rule, record the current problem in explicit terms.

At minimum, state:
- what happened
- what should not have happened
- what evidence supports that conclusion
- whether the problem is single-occurrence or repeated
- whether it is local to the current project instance or shared across the starter skeleton

Do not begin by naming a new rule. Begin by describing the observed problem.

## Current Problem Analysis
Analyze the problem before choosing a target document.

At minimum, determine whether the problem is primarily:
- `structure problem`: hierarchy, file placement, routing, or artifact misuse
- `scope problem`: the governed target or boundary is unclear or drifted
- `contract problem`: inputs, outputs, interfaces, or runner expectations are unclear or incorrect
- `oracle problem`: correctness cannot be judged clearly enough
- `failure problem`: recurring failure modes are missing, hidden, or misclassified
- `entry problem`: agents are entering the work in the wrong order or with the wrong read-first behavior
- `execution tracking problem`: the work now needs plans, references, or tracker updates
- `discipline problem`: the rule exists, but the agent bypassed it without a structural reason

Then determine which of these is true:
- `missing rule`: no governing rule currently covers the problem
- `weak rule`: a rule exists, but it is too weak or too implicit
- `misplaced rule`: a rule exists, but it lives at the wrong layer
- `ignored rule`: the rule exists and is adequate, but the agent failed to follow it
- `mechanical gap`: the rule is repeated often enough that prose may no longer be sufficient
- `genericization gap`: starter form was preserved, but project-specific runtime meaning was not extracted strongly enough

Do not promote a problem upward until this analysis is explicit.

## Existing Rule First Check
Before adding a new rule, check the existing governing documents first.

At minimum, review:
- `HARNESS_CONSTITUTION.md`
- the relevant file in `docs/harness/*`
- `AGENTS.md` if the issue concerns read-first order or short entry guardrails
- `plans/tech-debt-tracker.md` if the issue may already be tracked

Ask these questions:
1. Does an existing rule already govern this problem?
2. Can the existing rule be clarified or strengthened instead of adding a new one?
3. Is the problem actually caused by ignoring an existing rule rather than by missing rules?
4. Would a local amendment solve the issue without changing higher-order structure?

The default action is amendment of existing governing text, not creation of parallel rules.

## Owning Layer Decision
After the existing-rule check, identify the owning layer.

Use this ownership map:
- constitutional structure and invariant starter protections -> `HARNESS_CONSTITUTION.md`
- starter carry-forward and project-instance preservation rules -> `STARTER_SPECIALIZATION_RULES.md`
- scope and repository boundary -> `docs/harness/HARNESS_SCOPE.md`
- inputs, outputs, execution flow, regression expectations, readiness -> `docs/harness/INPUT_OUTPUT_CONTRACT.md`
- structural and behavioral judgment -> `docs/harness/ORACLE_AND_JUDGMENT.md`
- failure classes and recovery interpretation -> `docs/harness/FAILURE_TAXONOMY.md`
- artifact structure, approval, synchronization, directory growth, law/artifact/entry separation -> `docs/harness/REPOSITORY_ARTIFACT_RULES.md`
- short read-first routing and short entry guardrails -> `AGENTS.md`
- repeated debt, unresolved drift, promotion candidates -> `plans/tech-debt-tracker.md`
- multi-step active corrective work -> `plans/active/*`
- durable non-authoritative supporting context -> `references/*`

Do not place a rule in a lower layer when a higher layer owns the meaning.

Do not move a local project problem upward into the constitution unless the problem exposes a genuinely shared structural gap.

## Rule Addition Ladder
Use the smallest sufficient target first.

Default ladder:
1. no rule addition
2. tracker entry only
3. local law amendment
4. short `AGENTS.md` guardrail addition
5. constitutional amendment
6. mechanical enforcement promotion candidate

Guidance:
- choose `no rule addition` when the issue is one-off and adequately covered already
- choose `tracker only` when the issue is real but not yet stable enough for a governing rule
- choose `local law amendment` when the meaning belongs to a specific law document
- choose `AGENTS.md` only for short entry-order or read-first corrections
- choose `constitutional amendment` only for shared structural gaps or starter invariants
- choose `mechanical enforcement promotion` when repeated review is no longer reliable enough

For existing-project bootstrap failures:
- choose at least `local law amendment` when concrete repository facts are visible but law text remains generic
- choose at least `tracker entry only` when known discrepancies exist but remain untracked

## Placement Rules
Add the corrective rule at the smallest layer that owns the meaning.

Examples:
- if the problem is “agents keep treating root design docs as authoritative,” strengthen `HARNESS_SCOPE.md` or `REPOSITORY_ARTIFACT_RULES.md`
- if the problem is “agents collapse structural and behavioral oracle into one vague section,” strengthen `ORACLE_AND_JUDGMENT.md`
- if the problem is “project instances keep dropping starter protections during rewriting,” strengthen `STARTER_SPECIALIZATION_RULES.md`
- if the problem is “agents create new directories without checking current classes first,” strengthen `REPOSITORY_ARTIFACT_RULES.md`
- if the problem is “agents skip required read-first order,” add or strengthen a short `AGENTS.md` guardrail
- if the problem is repeated but not yet stable enough for law, record it in `plans/tech-debt-tracker.md`
- if the problem is “the bootstrap preserved starter structure but failed to extract concrete local facts,” strengthen `BOOTSTRAP_PROJECT_PROMPT.md`, `STARTER_SPECIALIZATION_RULES.md`, or the affected project law document

Do not add a new root file or new artifact class when an existing governing file already owns the problem.

## Do Not Add Rules When
Do not add a new rule when:
- the problem is a single mistake with no sign of repetition
- an existing rule already governs the issue adequately
- the proposed text would only duplicate an existing rule
- the issue is personal preference rather than governance meaning
- the issue belongs to a project-local law document, but you are trying to move it into the constitution
- the proposed addition would turn `AGENTS.md` into a rule store
- the issue should instead be solved by following the current law more carefully

## Addition Without Deletion Rule
Corrective rule addition must preserve the existing governed structure.

The default action is:
- preserve the current document
- preserve its existing protections
- add or strengthen only the text needed to address the current problem

Do not silently delete existing protections while adding a new one.

Deletion or removal is allowed only when:
- the text is genuinely redundant with a clearer rule in the same owning layer
- the text conflicts with a higher-order governing rule
- the replacement is explicit and materially stronger or clearer
- the impact of deletion is reviewed rather than assumed

Project-specific specialization must follow the starter law invariants and must not silently weaken them through summarization.

## Rule Addition Entry Shape
When recording or applying a corrective addition, make these items explicit:
- `problem`
- `evidence`
- `analysis result`
- `repetition status`
- `existing rule first check result`
- `owning layer`
- `chosen target`
- `why not lower`
- `why not higher`
- `whether tracker follow-up is needed`
- `whether mechanical enforcement should later be considered`

This entry shape may appear in review notes, tracker entries, or amendment rationale.

## Review Loop
After adding or strengthening a rule, review the result.

Check:
- does the new text actually address the current problem
- does it preserve existing protections
- is it placed at the correct layer
- did it avoid unnecessary escalation
- did it keep `AGENTS.md` short
- did it avoid creating a new artifact class unnecessarily
- does another related document also need synchronization
- for existing-project work, did it convert visible local repository facts into readable law rather than leaving them implicit in code
- for existing-project work, do known discrepancies remain explicitly documented in law or tracker form

If the answer to any of these is no, correct the placement before continuing.

## Existing-Project Bootstrap Completion Rule
When bootstrap or rebuild work targets an existing repository with meaningful code or runtime artifacts, the work is incomplete unless all of the following are true:
- starter structure and protections remain intact
- the law layer contains concrete repository-specific facts rather than only starter-level generalities
- known code/UI/doc mismatches remain explicitly documented
- repeated, material, or mechanically detectable drift is either tracked or explicitly justified as not needing a tracker entry yet

## Reference Update Completion Rule
If the corrective work adds, removes, splits, renames, or repositions a governing document, the work is not complete until affected references are updated.

At minimum, review whether updates are needed in:
- `AGENTS.md` read-first order
- `HARNESS_CONSTITUTION.md`
- the relevant files in `docs/harness/*`
- root-level bootstrap or control-loop documents
- tracker or plan references when they point to the changed document set

Moving text without updating its read path, ownership references, or synchronization points counts as an incomplete change.

If a document split or relocation occurs, the agent must explicitly confirm:
- which document now owns the moved meaning
- which documents still point to the old location
- that those references have been updated or intentionally deferred with visible impact

## Mechanical Enforcement Promotion
If the same problem or review rule recurs often enough, prose-only correction may no longer be sufficient.

If the current problem is:
- repeated
- high-cost or high-risk
- and mechanically detectable

then the default response should be enforcement promotion rather than leaving the prohibition as prose-only guidance.

Promotion candidates should be recorded when:
- the same structural mistake appears repeatedly
- agents continue to bypass an explicit rule
- the repository needs a stable automated check for a known invariant

Likely promotion targets include:
- lint rules
- CI checks
- structural tests
- document existence checks
- cross-reference checks

When feasible, prefer mechanisms that make the forbidden path fail directly rather than mechanisms that merely remind agents not to try it.

Record repeated candidates in `plans/tech-debt-tracker.md` before or while designing stronger enforcement.

If a current problem concerns implementation or repository structure, the agent must evaluate whether it is mechanically detectable.

If it is mechanically detectable, the agent must treat executable enforcement as the default path and must not leave the prohibition as prose-only guidance without explicit justification.

Detailed executable-enforcement policy lives in:
- `MECHANICAL_ENFORCEMENT_POLICY.md`

## Next Update Trigger
Update this document when:
- the problem-analysis process proves incomplete
- rule-placement mistakes recur
- the rule-addition ladder needs refinement
- repeated incidents show that stronger promotion guidance is needed
