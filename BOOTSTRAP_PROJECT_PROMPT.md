# Bootstrap Project Prompt

## Purpose
Use this document when a coding agent is asked to initialize Harness governance in a target repository.

This document is a shared bootstrap entry point for any coding agent. It is not itself a law document. Its role is to start the repository in the correct order and route work into the real governing files.

## Bootstrap Prompt
Use the following prompt, adapted only as needed for the target repository path or user intent:

```text
You are initializing this repository with the Harness starter skeleton.

Your job is to set up or refresh the harness governance before implementation work.

Follow these rules:
- Treat `HARNESS_CONSTITUTION.md` and `docs/harness/*` as the governing scaffold you are instantiating, not as disposable prose.
- Project-specific specialization is an overlay on the starter structure, not a replacement of starter protections.
- Keep `AGENTS.md` short. Do not turn it into a rule store.
- Use the current target repository itself as the authoritative product source.
- If the repository already contains code, docs, or runtime files, use them as the primary evidence for project meaning.
- If old design docs exist, treat them as reference-only unless the law layer explicitly promotes something out of them.
- Do document impact analysis before implementation.

First classify the target repository:
1. `empty or near-empty project`
2. `existing project with meaningful code or runtime artifacts`

If the repository is empty or near-empty:
- instantiate the starter law layer
- fill `docs/harness/*` around the intended first-release boundary
- keep unknowns explicit
- do not invent extra structure unless the starter rules require it

If the repository already has meaningful code or runtime artifacts:
- instantiate the starter law layer
- preserve the starter structure and starter protections while specializing it into project law
- treat the current code and file structure as the authoritative behavioral baseline
- rewrite `docs/harness/*` from current repository facts rather than from legacy planning prose
- extract concrete project-specific runtime facts into the law layer instead of leaving the result at generic starter wording
- make known mismatches between code, UI, docs, and expectations explicit
- record concrete known drift in `plans/tech-debt-tracker.md` when the repository already contains repeated, material, or mechanically detectable mismatches
- use `plans/*` only when the recovery or implementation work is genuinely multi-step
- use `references/*` only for durable non-authoritative material

In both cases:
- preserve starter form and starter protections while filling that form with repository-local facts
- preserve starter-level structural protections in `REPOSITORY_ARTIFACT_RULES.md`
- preserve both structural and behavioral oracle layers in `ORACLE_AND_JUDGMENT.md`
- document scope, contract, oracle, failure handling, execution flow, and regression expectations before implementation
- do not begin implementation until the documentation gate is satisfied for the affected area

Additional requirements for existing projects:
- do not leave project laws at starter-level generality when concrete runtime facts are already visible in code or files
- do not treat `browser app`, `local persistence`, or similarly generic summaries as sufficient when the repository exposes more specific governed behavior
- if the current repository exposes concrete mismatches, unstable semantics, or misleading controls, document those discrepancies explicitly in the law layer rather than smoothing them away
- if known drift exists and the tracker remains empty, treat the bootstrap as incomplete unless the absence is explicitly justified
- do not document every visible discrepancy by default; document the discrepancies that materially affect governed meaning, review safety, or repeated future work

Your first deliverable is the law layer and short entry map, not product code.
```

## Dynamic Bootstrap Rules
Use this bootstrap entry point when the target repository falls into one of these modes:

- `empty`: almost no product files exist yet
- `near-empty`: repository structure exists, but product meaning is still mostly undefined
- `existing`: runtime files, code, or app behavior already exist and must be treated as evidence

The bootstrap must not assume that all target repositories start from zero.

## Expected Output Of A Good Bootstrap
A correct bootstrap should produce:

- `HARNESS_CONSTITUTION.md`
- `AGENTS.md`
- `docs/harness/*`
- `plans/*` only when complexity justifies them
- `references/*` only when durable repository-local references are needed

For an existing project, a correct bootstrap must also produce:
- project-specific law text that another agent could use without re-deriving core runtime facts from code
- explicit known mismatches when code, UI, docs, or expectations disagree
- non-empty tracked debt or promotion candidates when real repeated or structurally important drift already exists

It should not:

- trust legacy planning prose over actual code
- delete starter structural protections during project-specific specialization
- collapse structural and behavioral oracle into one vague section
- overload `AGENTS.md`
- preserve starter form while leaving project laws generic when concrete local facts are already available
- leave known project mismatches undocumented just because the starter structure is complete
- flood the law layer with low-importance implementation irregularities that do not materially affect governed meaning
- jump straight into implementation

## When To Reuse This Prompt
Reuse this prompt when:

- a new repository is being initialized
- an existing repository's harness must be rebuilt from current code
- earlier generated harness docs should be discarded and recreated from the latest starter skeleton
