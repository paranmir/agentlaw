# Starter Specialization Rules

## Purpose
This document defines how the Harness kit may be specialized into a project-specific repository without deleting or weakening starter-level protections by default.

It governs specialization of starter documents into project documents.
It is not a one-time bootstrap helper only.
It remains authoritative whenever starter law is being interpreted, preserved, merged, rebuilt, or updated into a project instance.

## Core Rule
Project-specific specialization is an overlay on the starter structure, not a replacement of that structure.

Project-specific law documents may add:
- local facts
- local examples
- stricter local rules
- runtime-specific detail
- project-specific workflow detail

They must not silently remove or materially weaken starter protections by summarization, omission, or replacement.

They also must not preserve starter form while leaving project law at generic starter-level wording when the target repository already exposes more concrete runtime facts.

## Starter Invariants
At minimum, the following starter protections are invariant and must carry forward unless a higher-order governing rule explicitly changes them:
- law-versus-operational-artifact-versus-execution-entry separation
- operational-artifact approval conditions
- directory-creation and expansion gates
- mandatory synchronization checks for structural changes
- continued structural-oracle existence
- the rule that document existence alone is not a sufficient behavioral oracle

## Mandatory Carry-Forward Protections
Project instances must keep these protections explicitly readable, not merely implied:
- execution-entry documents are routing-only rather than rule-owning documents
- operational artifacts are supporting rather than law-owning artifacts
- operational artifacts require approval conditions
- directory creation is gated rather than convenience-driven
- pre-creation checks must happen before new structure is introduced
- expansion decisions must prefer existing governed structure first
- synchronization checks are required when structure or meaning changes
- both structural and behavioral oracle layers remain usable for later review and implementation judgment
- project-specific law must become concretely more local as repository facts become available
- known runtime discrepancies must remain explicit rather than being smoothed into generic readiness prose

## Artifact-Law Preservation
When a project instance rewrites `REPOSITORY_ARTIFACT_RULES.md` in project-specific terms, it may add local artifact maps and local runtime examples.

It must still preserve:
- artifact approval rules
- naming and structure gates
- synchronization obligations
- law/artifact/entry separation

## Oracle Preservation
When a project instance rewrites `ORACLE_AND_JUDGMENT.md` in project-specific terms:
- structural oracle must remain present
- behavioral oracle must become more concrete as project meaning becomes clearer
- document existence alone must not become the final behavioral oracle
- both oracle layers must remain reviewable and usable

## Amendment, Not Replacement
The default specialization action is amendment of the relevant starter document, not creation of parallel meaning.

The default behavior is:
- preserve the owning document
- preserve its existing protections
- add or strengthen only the text needed for local meaning

For existing projects, this also means:
- replace generic starter placeholders with concrete repository facts when those facts are already visible
- prefer code-backed local detail over reusable starter wording
- treat undocumented known discrepancies as an incomplete specialization result
- avoid inflating the law layer with local irregularities that do not materially affect governed meaning or review safety

## Allowed Weakening Threshold
A project instance may weaken or remove one of the starter protections only when all of the following are true:
- the change is explicit
- the reason is justified
- the owning higher-order layer is identified
- the deletion impact is reviewed
- the result is still governable and not merely shorter

Silence is not permission.

## Read Path Rule
When initializing, rebuilding, or updating a project instance, read these root-level control documents before rewriting or merging project-specific laws:
1. `HARNESS_CONSTITUTION.md`
2. `HARNESS_INIT_TOOL.md` for bootstrap or rebuild flows
3. `HARNESS_UPDATE_TOOL.md` for incremental shared-kit update flows
4. `docs/harness/STARTER_SPECIALIZATION_RULES.md`
5. the relevant files in `docs/harness/*`
6. `HARNESS_FIX_TOOL.md` when the task comes from a current problem or repeated harness failure
7. `AGENTS.md`

## Lifetime And Ownership
This document continues to govern after initial bootstrap.

Use it whenever the question is how starter protections must survive:
- first bootstrap into a target repository
- later rebuild of localized law
- incremental shared-kit update merges
- corrective rewriting after a harness failure or drift event

Do not treat it as disposable after the first initialization run.
Its role is persistent interpretation of starter-law carry-forward, not project-specific runtime ownership.

## Genericization Failure Rule
Specialization is incomplete when it preserves the starter outline but fails to capture project-specific runtime meaning.

Treat the result as incomplete when one or more of the following is true:
- the resulting law could still describe many unrelated projects equally well
- known repository-specific discrepancies are visible in code but absent from the law layer
- concrete persistence, interface, or runtime semantics remain generic even though the repository exposes them directly
- the tracker stays empty despite visible repeated, material, or mechanically detectable discrepancies
- the project has multiple distinct features or entry points but the law layer documents only the primary one
- exposed inputs or configuration points exist that do not affect actual behavior, and this mismatch is not recorded
- a feature relies on output or state from another feature, and this dependency is not documented

It is not incomplete merely because every visible discrepancy in the codebase was not documented.

The failure is genericization or omission of important discrepancies, not the absence of exhaustive issue listing.

## Next Update Trigger
Update this document when:
- project instances keep dropping starter protections
- specialization rules prove ambiguous
- carry-forward protections need refinement
- a repeated specialization failure should be promoted into stronger enforcement
