# Harness Constitution

## 0. Purpose and Boundaries
`HARNESS_CONSTITUTION.md` is the top-level governing document for the Harness kit. It fixes stable rules for document structure, document authority, documentation gates, and change handling.

This document does not store project-specific real criteria, implementation detail, or local operating values. Those belong below the constitution layer.

These rules are intended to govern any coding agent working in the repository, not a single model-specific workflow.

## 1. Governance Hierarchy
The governance hierarchy is:

1. `HARNESS_CONSTITUTION.md`
2. root control documents
3. `docs/harness/*`
4. approved structured repository artifacts such as `plans/*`, `references/*`, and stable generated facts when they exist
5. `AGENTS.md` and other execution-entry documents

Rules:
- The constitution defines stable top-level rules.
- Root control documents are starter-level control documents that live beside the constitution and guide initialization and corrective-governance loops without replacing the law layer.
- Project-specific real working criteria belong in `docs/harness/*`.
- Structured repository artifacts may store durable planning, reference, or derived context, but they do not override the law layer.
- A structured repository artifact is approved only when it is created under the repository artifact rules and any required synchronization impact has been handled or explicitly recorded.
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
- Project-specific real criteria belong in `docs/harness/*`.
- Execution-entry documents may direct work, but may not create higher-order rules.
- Repeated stable constraints should later be promoted to mechanical enforcement when appropriate.
- Repeated prohibited actions that are mechanically detectable must not remain prose-only when executable enforcement is feasible.

## 3. Law Document System
The law layer defines the real working criteria for a project through `docs/harness/*`.

The minimum law-document categories are:
- `HARNESS_SCOPE`: scope, non-scope, and target boundaries
- `INPUT_OUTPUT_CONTRACT`: required inputs, outputs, and interface expectations
- `ORACLE_AND_JUDGMENT`: judgment logic, oracle rules, and acceptance criteria
- `FAILURE_TAXONOMY`: failure classes, error interpretation, and handling boundaries
- `REPOSITORY_ARTIFACT_RULES`: artifact naming, structure, and expansion rules

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

At minimum, invariant starter protections include:
- law-versus-operational-artifact-versus-execution-entry separation
- operational-artifact approval conditions
- directory-creation and expansion gates
- mandatory synchronization checks for structural changes
- continued structural-oracle existence
- the rule that document existence alone is not a sufficient behavioral oracle

If a project instance needs to weaken or remove one of these protections, that decision must be explicit, justified, and traceable to the owning higher-order governing layer rather than introduced by local summarization or replacement.

Detailed starter-specialization carry-forward rules live in:
- `docs/harness/STARTER_SPECIALIZATION_RULES.md`

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

## 5. Execution-Entry Documents
Execution-entry documents exist to help an agent enter the work and follow the governing documents correctly.

Rules:
- `AGENTS.md` is the primary execution-entry map.
- It should stay short, clear, and operational.
- It should point to governing documents and current work order.
- Structured operational artifacts may be referenced by execution-entry documents when they already exist or must be created under the governing rules.
- It must not become a rule encyclopedia or replace the constitution or laws.

## 5A. Root Control Documents
Root control documents are root-level control artifacts that work beside the constitution and before detailed law reading.

The default root control documents are:
- `BOOTSTRAP_PROJECT_TOOL.md`
- `PROBLEM_ANALYSIS_AND_RULE_ADDITION_TOOL.md`

Their role is to:
- help agents initialize or rebuild a repository from the shared harness kit
- analyze current governance problems and place corrective additions at the correct layer

They must not:
- replace `docs/harness/*` as the project law layer
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
