# Harness Constitution

## 0. Purpose and Boundaries
`AGENTLAW_CONSTITUTION.md` is the top-level governing document for the Harness kit. It fixes stable rules for document structure, document authority, documentation gates, and change handling.

This document does not store project-specific real criteria, implementation detail, or local operating values. Those belong below the constitution layer.

These rules are intended to govern any coding agent working in the repository, not a single model-specific workflow.

## 1. Governance Hierarchy
The repository follows a 5-layer written-governance structure. The hierarchy is:

1. Root-level constitutional documents (`AGENTLAW_CONSTITUTION.md`)
2. Root-level control documents (`AGENTLAW_INIT_TOOL.md`, `AGENTLAW_UPDATE_TOOL.md`, `AGENTLAW_FIX_TOOL.md`)
3. Law documents (`docs/law/*`)
4. Approved structured repository artifacts (`docs/contracts/*`, `docs/plans/*`, `docs/references/*`, and stable generated facts when they exist)
5. Approved continuity or memory records (`memory/*`)
6. Execution-entry documents (`AGENTS.md`) and `README.md`

Rules:
- The constitution defines stable top-level rules.
- Root control documents are starter-level control documents that live beside the constitution and guide initialization and corrective-governance loops without replacing the law layer.
- Project-specific real working criteria belong in `docs/law/*`.
- Structured repository artifacts may store durable planning, reference, or derived context, but they do not override the law layer.
- A structured repository artifact is approved only when it is created under the repository artifact rules and any required synchronization impact has been handled or explicitly recorded.
- Continuity or memory records are derived aids for restoring and assembling context. They do not override constitutional documents, root control documents, law documents, or approved structured repository artifacts.
- `plans/*` records governed execution intent and progress for meaningful work.
- `references/*` stores searchable repository-local context and does not create law-layer meaning by itself.
- `AGENTS.md` is an execution-entry map, not a source of truth.
- Execution-entry documents route work; operational artifacts support work; neither replaces the law layer.
- `RULES.md` is not used.

## 2. Constitutional Principles
The constitution layer fixes the following principles:

- Document structure comes before implementation.
- Constitution, laws, structured operational artifacts, and execution-entry documents must remain separate.
- Project-specific specialization of starter documents is an overlay on the governed structure, not a replacement of that structure.
- Repository growth must reuse existing approved artifact classes and paths before introducing new structure.
- New structure is allowed only when the current governed structure is shown to be insufficient.
- Stable shared rules belong in the constitution layer.
- Project-specific real criteria belong in `docs/law/*`.
- Execution-entry documents may direct work, but may not create higher-order rules.
- Repeated stable constraints should later be promoted to mechanical enforcement when appropriate.
- Repeated prohibited actions that are mechanically detectable must not remain prose-only when executable enforcement is feasible.
- Continuity and memory are official default Harness capabilities, but their runtime state must remain lower-authority derived context rather than governing law.

## 2A. Code Authorship And Stewardship
When writing, generating, modifying, refactoring, deleting, or reviewing code, agents must treat maintainability and testability as part of correctness.

Code work must be test-anchored: non-trivial code work requires executable tests, existing executable coverage, or focused verification tied to an explicit behavioral oracle.

Before non-trivial code work begins, agents must identify the affected contract surfaces, the behavioral or mechanical oracle, and the smallest safe implementation slice.

Verification evidence must match the affected surface. Evidence from one execution context must not be treated as sufficient when the changed behavior is exercised through another runtime, host, configuration layer, persistence layer, distribution path, or public contract.

Agents must not introduce avoidable technical debt from the first implementation. They must keep responsibilities narrow, dependencies explicit, behavior testable, and reuse local architecture before adding new abstractions.

SOLID principles and established design patterns may guide code structure when relevant, but they must not be used to justify speculative abstraction, broad rewrites, unnecessary class hierarchies, or pattern-shaped code without current need.

Code must be written for future review as well as execution. Agents must preserve the reasoning-critical context needed to understand requirements, boundaries, invariants, risks, and non-obvious design choices.

Non-trivial implementation work must leave enough durable context for a later human or agent to reconstruct the intended behavior, affected surfaces, verification oracle, and design constraints without relying on conversation history.

## 3. Law Document System
The law layer defines the real working criteria for a project through `docs/law/*`.

The minimum law-document categories are:
- `HARNESS_SCOPE`: scope, non-scope, and target boundaries
- `INPUT_OUTPUT_CONTRACT`: required inputs, outputs, and interface expectations
- `ORACLE_AND_JUDGMENT`: judgment logic, oracle rules, and acceptance criteria
- `FAILURE_TAXONOMY`: failure classes, error interpretation, and handling boundaries
- `REPOSITORY_ARTIFACT_RULES`: artifact naming, structure, and expansion rules
- `MEMORY_AND_CONTINUITY_RULES`: memory authority, layout, lookup, conflict, protocol, and session restore rules
- `CODE_AUTHORSHIP_AND_STEWARDSHIP_RULES`: code authorship, test anchoring, behavioral oracle use, maintainability, and technical-debt boundaries

Additional law documents may be required when the project needs them:
- `RUNNER_FLOW`
- `REGRESSION_STRATEGY`
- `STARTER_SPECIALIZATION_RULES`
- `MECHANICAL_ENFORCEMENT_POLICY`

Rule:
- `RUNNER_FLOW` and `REGRESSION_STRATEGY` are conceptually required before implementation.
- Their content must be documented before implementation starts.
- They are not always required as separate files and may be explicit sections inside other law documents.

## 3A. Starter Law Invariants
Some law-layer content in the shared starter kit is invariant scaffolding rather than optional drafting style.

Project-specific specialization may add local facts, local examples, and stricter local rules, but it must not silently remove or materially weaken invariant starter protections.

The constitution fixes the existence of these invariant starter protections, but the detailed carry-forward, read-path, and project-instance specialization rules do not belong here.

Those detailed specialization rules live in:
- `docs/law/STARTER_SPECIALIZATION_RULES.md`

## 4. Structured Knowledge Expansion
As a project grows, agents must not compensate by turning `AGENTS.md` into a larger rule store.

When the project's working knowledge outgrows the minimum law layer, that knowledge must be expanded into structured repository artifacts.

Allowed expansion targets include:
- versioned planning artifacts such as `plans/active/*` and `plans/completed/*`
- tracked debt or promotion candidates such as `plans/tech-debt-tracker.md`
- repository references such as `references/*`
- generated or derived repository facts when they become important and versionable

Rule:
- `AGENTS.md` stays short and map-like even after expansion.
- expansion must preserve existing governed structure unless insufficiency is made explicit and recorded at the correct layer
- Durable knowledge must move into structured repository artifacts rather than accumulating in chat or ad hoc notes.
- Repeated stable constraints should be promoted from prose into mechanical enforcement when practical.
- If a forbidden path is mechanically detectable through code, tests, lint, CI, or structural checks, the repository should prefer enforcement that fails the invalid path rather than relying on instruction-following alone.

## 4A. Continuity And Memory Boundary
Harness supports continuity and memory as official default capability classes.

Rules:
- Continuity and memory exist to help agents recover, assemble, and relate current context across sessions and tools.
- File-based governance remains authoritative over runtime memory state.
- Memory records, summaries, indexes, and context packets are derived continuity aids, not sources of governing meaning.
- Canonical memory, exact indexes, semantic indexes, runtime job/cache paths, protocol surfaces, and session restore behavior are default Harness subsystems.
- Default subsystem status does not require every runtime process to stay hot. Runtime components may start, rebuild, idle, or degrade to canonical memory files.
- When memory conflicts with repository files, the repository files win and the memory item must change status before it can affect ordinary retrieval.
- Memory may surface recursive-improvement candidates, but promotion into law, plans, references, mechanical enforcement, or shared artifacts requires the normal reviewed change path.
- Hidden runtime state must not become hidden authority. Any runtime data path used for memory must be governed by repository artifact rules before it becomes normal practice.

## 5. Execution-Entry Documents
Execution-entry documents exist to help an agent enter the work and follow the governing documents correctly.

Rules:
- `AGENTS.md` is the primary execution-entry map.
- It should stay short, clear, and operational.
- It should point to governing documents and current work order.
- Structured operational artifacts may be referenced by execution-entry documents when they already exist or must be created under the governing rules.
- It must not become a rule encyclopedia or replace the constitution or laws.
- Before adding content to an execution-entry document, classify whether the content is routing (read order, current priority, entry guardrails) or governing (scope, contract, oracle, failure, artifact structure). Governing content must be placed in the appropriate law document, not in the execution-entry document.

## 5A. Root Control Documents
Root control documents are root-level control artifacts that work beside the constitution and before detailed law reading.

The default root control documents are:
- `AGENTLAW_INIT_TOOL.md`
- `AGENTLAW_UPDATE_TOOL.md`
- `AGENTLAW_FIX_TOOL.md`

Their role is to:
- help agents initialize or rebuild a repository from the shared harness kit
- help agents incorporate upstream harness changes into an already-bootstrapped project
- analyze current governance problems and place corrective additions at the correct layer

They must not:
- replace `docs/law/*` as the project law layer
- silently redefine project-specific scope, contract, oracle, or failure meaning
- become substitutes for execution-entry documents or operational artifacts

## 6. Documentation Gate Before Implementation
Implementation must not start before the law layer documents the minimum criteria needed to govern the work.

Before implementation, the following must be documented somewhere in the law layer:
- scope boundaries
- input and output expectations
- judgment criteria
- failure classification
- execution flow and regression expectations

If these are missing, document impact analysis and document consistency recovery take priority over implementation.

## 7. Change Rules
New requests must not be treated as implementation tasks first.

The first check is whether the request changes:
- constitutional structure
- law-layer criteria
- execution-entry routing

If a request changes meaning, judgment, scope, contracts, failure handling, execution flow, or regression expectations, the governing documents must be updated first at the correct layer.

If the project grows in complexity such that plans, references, generated facts, or enforcement rules are needed, those repository structures must be added before relying on repeated undocumented practice.

## 8. Maintenance Rule
Document drift is a governance failure.

When conflict appears between layers:
- restore consistency at the correct layer
- do not delete starter-level structural protections during project-specific specialization unless a higher-order governing rule explicitly changes them
- do not patch around the problem in execution-entry documents
- do not move project-specific criteria upward into the constitution
