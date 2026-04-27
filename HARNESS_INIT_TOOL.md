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

## Full Bootstrap Cycle

The full bootstrap cycle has four steps. Step 0 is user-driven (terminal command). Steps 1-2 are LLM-driven and form the substance of this document. Step 3 is verification.

0. **Infrastructure setup (terminal)**: `pipx install rule-harness` (once, globally), then `cd <target-project>` and `rule-harness init [--setup-agents prompt|auto|none]`. The pip package unpacks the bundled scaffold (constitution, root control documents, generic starter `docs/harness/*`, `memory/*` seed) into the target repository and creates the runtime tree at `.harness/index/meta.db`. Optional sub-steps: download the embedding model, register the harness-memory MCP server with the agent host (depending on `--setup-agents` mode).
1. **Law specialization (LLM-driven)**: invoke an LLM with this document. The LLM follows the Direct Procedure below — classify the target, specialize the generic starter law (already on disk from step 0) into project-specific law, record the shared baseline, document scope/contract/oracle/failure/execution/regression/memory authority before implementation. This is the substance of the bootstrap.
2. **Runtime context load (LLM-driven, applicable when MCP is registered)**: after registration in step 0 (if `--setup-agents prompt` or `auto`) and after specialization in step 1, the LLM calls `harness_session_restore` (MCP) or `rule-harness session-restore` (CLI fallback). The response carries the runtime reminder packet — authority recall, write discipline, memory intent rule with multilingual trigger keywords. Until this step has run, the agent has the law layer specialized but no runtime reminders loaded into working context.
3. **Verification (terminal)**: `rule-harness verify` (or `python verify_harness.py` before the package command is available). Confirms the law layer, baseline record, and memory subsystem are coherent.

The Direct Procedure below is a self-contained version of step 1, beginning with a step 0 reminder. The Responsibility Split and Non-Pip Distribution sections at the bottom of this document supplement the cycle for non-default situations.

## Direct Procedure
Use this document as a direct procedure even if no prompt block is reused.

0. Run `pipx install rule-harness` (once, globally) and `rule-harness init` in the target project directory before invoking the LLM-driven steps. The pip package places the bundled scaffold (constitution, root control documents, generic starter `docs/harness/*`, `memory/*` seed) on disk and creates `.harness/index/meta.db`. The LLM-driven steps below specialize that scaffold for the target project. Skip step 0 only when the project does not use the pip package distribution and the shared kit is delivered through another channel (git clone, manual copy) — see Non-Pip Distribution at the bottom.
1. Classify the repository as `empty or near-empty project` or `existing project with meaningful code or runtime artifacts`.
2. Specialize the starter law layer (the pip package's scaffold copy in step 0 placed the generic starter on disk; this step specializes that text without deleting starter protections). For empty / near-empty projects, fill `docs/harness/*` around the intended first-release boundary while keeping unknowns explicit. For existing projects, rewrite `docs/harness/*` from current repository facts rather than from legacy planning prose.
3. For existing projects, derive project law from current repository facts rather than legacy planning prose.
4. Record the shared-harness baseline in `references/shared-harness-baseline.md` when the repository is being bootstrapped from a shared-kit commit or tag that can be identified.
5. Document scope, contract, oracle, failure handling, execution flow, regression expectations, and memory authority before implementation.
6. Treat memory, indexes, protocol surfaces, runtime jobs/cache, and session restore as default Harness subsystems below law.
7. Record important mismatches and repeated or structurally important drift in the tracker when needed.
8. Keep `AGENTS.md` short and routing-only.
9. Treat the law layer and short entry map as the first deliverable, not product code.
10. Load runtime reminders. After law specialization, register the harness-memory MCP server (when applicable — typically by following the instructions emitted by `rule-harness init --setup-agents prompt`, or by running `rule-harness agent-setup --client <host> --target . --apply --yes` for direct config writes), then call `harness_session_restore` (MCP) or `rule-harness session-restore --target . --json` (CLI fallback). The response carries authority recall, write discipline, and memory intent rule reminders — including the multilingual trigger keyword list. The session is not "fully bootstrapped" until these reminders are loaded into the agent's working context, since otherwise the agent operates without the framework guidance the rest of the harness depends on.

## Completion Checks
A bootstrap run is complete only when:
- the governed law layer exists
- starter protections remain readable
- the resulting law is project-local rather than generic starter wording
- the shared baseline record exists or an explicit reason for deferral is visible
- important mismatches are explicit when they materially affect governed meaning or review safety
- implementation has not started ahead of the documentation gate
- memory runtime state is documented as derived continuity below repository files and law

## Failure Conditions
Treat the bootstrap as failed or incomplete when:
- starter protections were removed during specialization
- project law stayed generic even though repository facts were visible
- a shared-kit bootstrap skipped baseline recording without explicit deferral
- only the primary feature was documented while other distinct entry points remained unguided
- exposed inputs or configuration points were accepted without checking whether they affect behavior
- implementation started before the law layer was ready
- memory runtime paths were created or trusted without artifact-rule and authority-boundary coverage

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
- specialize the starter law layer (the pip package's scaffold copy already placed the generic starter on disk; this step specializes that text)
- fill `docs/harness/*` around the intended first-release boundary
- keep unknowns explicit
- do not invent extra structure unless the starter rules require it

If the repository already has meaningful code or runtime artifacts:
- specialize the starter law layer (the pip package's scaffold copy already placed the generic starter on disk; this step specializes that text)
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
- when the shared-kit source commit or tag is identifiable, create `references/shared-harness-baseline.md` and record it as the current bootstrap baseline
- treat memory as official derived continuity rather than law; document its runtime path and conflict behavior before use
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

## Default Memory Subsystem Rule
Harness bootstrap includes default memory and continuity structure.

Memory, exact indexes, semantic indexes, runtime job/cache paths, protocol surfaces, and session restore behavior are default Harness subsystems. They do not make memory authoritative.

Do not trust memory runtime state merely because an installer creates it. The target law layer must preserve the memory authority boundary and the artifact rules must cover runtime paths before memory becomes normal operation.

## Expected Output Of A Good Bootstrap
A correct bootstrap should produce:

- `HARNESS_CONSTITUTION.md`
- `AGENTS.md`
- `docs/harness/*`
- `plans/*` only when complexity justifies them
- `references/*` only when durable repository-local references are needed

A correct bootstrap may also produce the canonical memory path `memory/*` — the human-reviewable Markdown layer (working-set, known-facts, logs, preferences, lookup rules) seeded from the shared kit. The derived runtime tree `.harness/*` (SQLite index `meta.db`, embedding model artifacts, caches, jobs) is produced by the `rule-harness` pip package's install/init step, not by this LLM-driven tool. Do not author or edit `.harness/*` contents directly from this flow; see the Responsibility Split section below for the boundary between the pip package and the LLM-driven bootstrap.

When the shared-kit source baseline is identifiable, a correct bootstrap should also produce:
- `references/shared-harness-baseline.md`

First-time init leaves the `project-overview` reference's "Code architecture map" subsection as the shipped template placeholder (no `Map scope:` glob declared yet). Populate it later, once the project has code worth mapping; until then the harness verifier silently skips the Layer 2 freshness check instead of forcing a FAIL on a brand-new target.

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
- treat memory records, installer manifests, or runtime state as higher authority than repository files

## When To Reuse This Prompt
Reuse this prompt when:

- a new repository is being initialized
- an existing repository's harness must be rebuilt from current code
- earlier generated harness docs should be discarded and recreated from the latest shared harness kit

## Responsibility Split

| Layer | Owner | What it does |
| --- | --- | --- |
| Pip package install | `pipx install rule-harness` | Makes the CLI / MCP entry points available globally |
| Scaffold copy onto disk (`HARNESS_CONSTITUTION.md`, `HARNESS_*_TOOL.md`, generic starter `docs/harness/*`, `memory/*` seed) | `rule-harness init` | Lays down shared scaffolding without project specialization |
| Runtime tree creation (`.harness/index/meta.db`, embedding artifacts, caches) | `rule-harness init` | Initializes the binary memory subsystem at the canonical location |
| Optional embedding model download | `rule-harness init` (when network and `--download-model` flag allow) | Pre-warms semantic search; skipping it leaves the system in FTS-only mode until the model arrives |
| Optional MCP server registration with the agent host | `rule-harness init --setup-agents prompt\|auto` | Either emits LLM-actionable instructions (`prompt`) or writes the host config directly (`auto`) |
| Law specialization (`docs/harness/*` rewrite from project facts), shared baseline record, scope/contract/oracle/failure/execution/regression/memory authority documentation | LLM via this document | Project-specific specialization on top of the generic starter |
| Runtime context load (`harness_session_restore`) | LLM (after MCP registration) | Pulls authority recall, write discipline, memory intent reminders into the agent's working context |
| Final integrity verification | User via `rule-harness verify` | Confirms the bootstrap produced a coherent state |

The split is hard: the LLM does not create or alter `.harness/index/meta.db` directly under any circumstance, and the pip package does not perform law specialization. Step 1 of the cycle (LLM specialization) must run only after step 0 (pipx install + rule-harness init) has completed cleanly — running step 1 without the scaffold on disk produces files that drift from the shared starter structure.

## Non-Pip Distribution

When the project does not use the pip-package distribution (the shared kit was delivered through git clone, archive download, or manual copy), step 0 of the cycle changes:

- Replace `pipx install rule-harness` + `rule-harness init` with whatever places the shared kit on disk (e.g., `git clone`, manual file copy, archive extraction).
- The runtime tree at `.harness/index/meta.db` must be created by another mechanism documented at the project level (manual `python -m harness_kit.cli init` against a local checkout, scripted equivalent, etc.). The LLM still does not create or alter `.harness/index/meta.db` directly.
- MCP server registration must be performed manually if the project still wants to use the MCP route; otherwise the agent operates in CLI-fallback mode.

The LLM-driven step 1 (this document's Direct Procedure), step 2 (runtime context load), and step 3 (verification) are unchanged.

## Next Update Trigger
Update this document when:
- the bootstrap process proves unclear in real use
- the responsibility split between pip package and LLM specialization needs refinement
- new shared subsystems need explicit bootstrap routing
