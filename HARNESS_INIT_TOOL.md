# Harness Init Tool

## Purpose
Use this document when a coding agent is asked to initialize Harness governance in a target repository.

This document is a shared bootstrap entry point for any coding agent. It is not itself a law document. Its role is to start the repository in the correct order and route work into the real governing files.

## Required Inputs
Before using this document, read:
- `HARNESS_CONSTITUTION.md`
- `docs/harness/STARTER_SPECIALIZATION_RULES.md`
- the current target repository files that expose actual product or runtime meaning
- `AGENTS.md` when a short execution-entry map already exists

## Direct Procedure
Use this document as a direct procedure even if no prompt block is reused.

1. Classify the repository as `empty or near-empty project` or `existing project with meaningful code or runtime artifacts`.
2. Instantiate the starter law layer without deleting starter protections.
3. For existing projects, derive project law from current repository facts rather than legacy planning prose.
4. Record the shared-harness baseline in `references/SHARED_HARNESS_BASELINE.md` when the repository is being bootstrapped from a shared-kit commit or tag that can be identified.
5. Document scope, contract, oracle, failure handling, execution flow, and regression expectations before implementation.
6. Record important mismatches and repeated or structurally important drift in the tracker when needed.
7. Keep `AGENTS.md` short and routing-only.
8. Treat the law layer and short entry map as the first deliverable, not product code.

## Completion Checks
A bootstrap run is complete only when:
- the governed law layer exists
- starter protections remain readable
- the resulting law is project-local rather than generic starter wording
- the shared baseline record exists or an explicit reason for deferral is visible
- important mismatches are explicit when they materially affect governed meaning or review safety
- implementation has not started ahead of the documentation gate

## Failure Conditions
Treat the bootstrap as failed or incomplete when:
- starter protections were removed during specialization
- project law stayed generic even though repository facts were visible
- a shared-kit bootstrap skipped baseline recording without explicit deferral
- only the primary feature was documented while other distinct entry points remained unguided
- exposed inputs or configuration points were accepted without checking whether they affect behavior
- implementation started before the law layer was ready

## Bootstrap Prompt
Use the following prompt only as an adapter when a model benefits from a direct handoff block.
The governing procedure is the document body above, not the fenced block by itself.

```text
You are initializing this repository with the Harness kit.

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
- make known mismatches between code, interface, docs, and expectations explicit
- record concrete known drift in `plans/tech-debt-tracker.md` when the repository already contains repeated, material, or mechanically detectable mismatches
- use `plans/*` only when the recovery or implementation work is genuinely multi-step
- use `references/*` only for durable non-authoritative material

In both cases:
- preserve starter form and starter protections while filling that form with repository-local facts
- when the shared-kit source commit or tag is identifiable, create `references/SHARED_HARNESS_BASELINE.md` and record it as the current bootstrap baseline
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
- when the project has multiple distinct features or entry points, document each feature's runtime contract — not just the primary flow; a bootstrap that covers only the main feature while leaving secondary features undocumented is incomplete
- cross-check exposed inputs and configuration points against their actual code consumption; an input that is accepted or displayed but does not affect behavior is a mismatch that must be documented
- when a feature uses simplified or partial logic (fixed constants, ignored parameters, hardcoded assumptions), document those simplifications explicitly so that another agent does not mistake them for configurable behavior

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

When the shared-kit source baseline is identifiable, a correct bootstrap should also produce:
- `references/SHARED_HARNESS_BASELINE.md`

For an existing project, a correct bootstrap must also produce:
- project-specific law text that another agent could use without re-deriving core runtime facts from code
- explicit known mismatches when code, interface, docs, or expectations disagree
- non-empty tracked debt or promotion candidates when real repeated or structurally important drift already exists
- runtime contract coverage for each distinct feature or entry point, not just the primary one
- documented simplifications when a feature uses fixed constants, ignored parameters, or partial logic that differs from what the exposed interface suggests

It should not:

- trust legacy planning prose over actual code
- delete starter structural protections during project-specific specialization
- collapse structural and behavioral oracle into one vague section
- overload `AGENTS.md`
- preserve starter form while leaving project laws generic when concrete local facts are already available
- leave known project mismatches undocumented just because the starter structure is complete
- flood the law layer with low-importance implementation irregularities that do not materially affect governed meaning
- document only the primary feature when multiple distinct features or entry points exist in the project
- accept exposed inputs or configuration points without verifying that they actually affect behavior
- jump straight into implementation

## When To Reuse This Prompt
Reuse this prompt when:

- a new repository is being initialized
- an existing repository's harness must be rebuilt from current code
- earlier generated harness docs should be discarded and recreated from the latest shared harness kit
