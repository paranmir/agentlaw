# Harness Update Tool

## Purpose
Use this document when the shared harness kit has been updated and a target project must incorporate the new or changed rules into its existing localized law layer.

This document is a shared update entry point for any coding agent. It is not itself a law document. Its role is to merge upstream harness changes into an already-bootstrapped project without losing local facts.

## When To Use This Document
Use this document when:
- the shared harness kit received new rules, requirements, or structural changes since the project was last bootstrapped or updated
- the project already has a localized law layer from a prior bootstrap
- the goal is incremental incorporation, not a full rebuild

Do not use this document when:
- the project has never been bootstrapped — use `HARNESS_INIT_TOOL.md` instead
- the goal is to discard and recreate the law layer from scratch — use `HARNESS_INIT_TOOL.md` instead
- a specific governance problem needs analysis — use `HARNESS_FIX_TOOL.md` instead

## Full Update Cycle

The full update cycle has three steps. Steps 1 and 3 are user-driven (terminal commands). Step 2 is LLM-driven and is the substance of this document.

1. **Infrastructure update (terminal)**: `pipx upgrade harness-kit`. Replaces the pip package code (CLI, MCP server, schema files), brings the new bundled shared kit into the package's `scaffold/` directory, and automatically applies any pending schema migrations to `.harness/index/meta.db`.
2. **Governance content merge (LLM-driven)**: invoke an LLM with this document. The LLM follows the Direct Procedure below — read the recorded baseline, compare new shared kit against it, replace root mirrors, merge new requirements into local law without losing local facts, refresh the baseline record.
3. **Verification (terminal)**: `harness-kit verify` (or `python verify_harness.py` before the package command is available). Confirms root mirrors match the new shared kit, local facts and behavioral oracle content were preserved, new shared requirements are present, and the shared baseline record matches.

The Direct Procedure below is a self-contained version of step 2 that begins with a step 0 reminder to run step 1 first. The Responsibility Split, Failure Modes, and Non-Pip Distribution sections at the bottom of this document supplement the cycle for non-default situations.

## Required Inputs
Before using this document, read:
- `HARNESS_CONSTITUTION.md`
- `references/shared-harness-baseline.md` when it exists
- the current localized law layer in `docs/harness/*`
- the new shared-kit versions of the root and law documents
- `AGENTS.md` when routing may need to change

## Direct Procedure
Use this document as a direct procedure even if no prompt block is reused.

0. Run `pipx upgrade harness-kit` first when the project is using the pip-package distribution. This step (a) replaces the pip package code (CLI, MCP server, schema files), (b) brings the new bundled shared kit into the package's `scaffold/` directory, and (c) automatically applies any pending schema migrations to `.harness/index/meta.db`. After this completes, proceed with the LLM-driven steps below. Skip step 0 only when the project does not use the pip package and the shared kit is delivered through another channel (git clone, manual copy).
1. Read the recorded shared baseline if available.
2. Identify shared-kit changes relative to the prior baseline, or fall back to law-gap comparison if no precise baseline exists.
3. Replace root mirror files with the new shared versions.
4. Merge new shared requirements into localized law without losing local facts.
5. Review tracker and enforcement implications.
6. Refresh the shared baseline record to the shared version now reflected in the project.
7. Update `AGENTS.md` routing if the read path changed.
8. Verify that every new shared requirement is traceable in the resulting localized law.

## Completion Checks
An update run is complete only when:
- root mirrors match the new shared kit
- localized law preserves prior local facts while incorporating relevant new shared requirements
- no localized law text reverted to generic starter wording
- existing localized law documents were checked for genericization gaps — starter placeholder sections that remain unfilled despite concrete project facts being available
- the shared baseline record matches the shared version now reflected in the project
- routing and tracker follow-up were reviewed when affected

## Failure Conditions
Treat the update as failed or incomplete when:
- it behaves like a full bootstrap and overwrites localized law
- local facts, local mismatches, or behavioral oracle content are lost
- new shared requirements remain unaddressed
- the shared baseline record is skipped without explicit deferral
- routing changed but `AGENTS.md` was left stale

## Update Prompt
Use the following prompt only as an adapter when a model benefits from a direct handoff block.
The governing procedure is the document body above, not the fenced block by itself.

```text
You are updating the harness governance in this repository to incorporate changes from a newer version of the shared harness kit.

Your job is to merge new shared requirements into the existing localized law layer without losing project-specific facts.

Follow these rules:
- The existing localized law layer is the starting point, not the new shared starter templates.
- New shared requirements must be incorporated, but existing local facts, local mismatches, local tracker entries, and local behavioral oracle content must be preserved.
- Do not re-derive local facts from code unless a new shared requirement specifically demands new code inspection.
- Do not replace localized law documents with fresh starter templates.
- Do not treat this as a bootstrap. The project already has project-specific law.
- Prefer the recorded shared-harness baseline in `references/shared-harness-baseline.md` when identifying the previous shared version.

Procedure:

0. If the project uses the pip-package distribution, run `pipx upgrade harness-kit` before invoking the LLM-driven steps. The pipx upgrade replaces pip package code, brings the new bundled shared kit into the package, and automatically applies any pending schema migrations to `.harness/index/meta.db`. Skip this step only when the shared kit is delivered through another channel.

1. Identify what changed in the shared kit.
   - First read `references/shared-harness-baseline.md` if it exists.
   - Compare the new shared kit files against the baseline commit or tag the project was last bootstrapped or updated from.
   - If the baseline file is missing or incomplete, read the new shared kit files and compare their requirements against the current project law layer to identify gaps.
   - Categorize each change as: new requirement, strengthened requirement, relaxed requirement, structural change, or wording-only clarification.

2. Replace root-level mirror files.
   - `HARNESS_CONSTITUTION.md`: replace with the new version.
   - Root control documents (`HARNESS_INIT_TOOL.md`, `HARNESS_FIX_TOOL.md`, `HARNESS_UPDATE_TOOL.md`): replace with the new versions.
   - These are shared scaffolding, not localized content, so direct replacement is correct.

3. Merge changes into the localized law layer.
   - For each file in `docs/harness/*`, compare the new shared starter template against the current project-specific version.
   - Add new sections or requirements from the shared template that the project law does not yet address.
   - Strengthen existing sections when the shared template now requires more than the project law currently provides.
   - Preserve all existing local facts, local examples, local mismatches, and project-specific behavioral oracle content.
   - Do not weaken local rules that are already stricter than the new shared requirement.
   - Do not revert project-specific wording back to generic starter wording.

4. Review tracker and enforcement entries.
   - Check whether any existing tracker entries are now governed by the new shared rules and can be promoted or resolved.
   - Check whether new shared requirements create new tracker candidates for the project.
   - Update `docs/harness/MECHANICAL_ENFORCEMENT_POLICY.md` if new enforcement-relevant requirements appeared.

5. Refresh the shared baseline record.
   - Update `references/shared-harness-baseline.md` to record the shared source repository and the commit or tag now reflected in the project.
   - If the exact baseline is still uncertain, keep the uncertainty explicit in the notes.

6. Update `AGENTS.md` if needed.
   - If the read-first order changed (e.g., a new root control document was added), update the routing.
   - Keep `AGENTS.md` short and routing-only.

7. Verify completeness.
   - Every new shared requirement should be traceable in the updated project law layer.
   - No local fact should have been lost during the merge.
   - No localized law document should have reverted to generic starter wording.
   - The update should not have introduced any project-type-specific language from the shared kit that does not apply to this project.
   - The shared baseline record should match the shared version now reflected in the project.
```

## Expected Output Of A Good Update
A correct update should produce:
- updated root-level mirror files matching the new shared kit
- localized law documents that incorporate new shared requirements while preserving all prior local facts
- reviewed tracker entries with promotions or new items when applicable
- refreshed `references/shared-harness-baseline.md`
- updated `AGENTS.md` routing if the read-first order changed
- when the update introduces or modifies the "Code architecture map" obligation in shared law, populate or refresh the corresponding section in the local `project-overview` reference before the next `harness-kit verify`; leaving the `Map scope:` block undeclared keeps the Layer 2 freshness check in skip mode

It should not:
- replace localized law documents with fresh starter templates
- lose project-specific facts, mismatches, or behavioral oracle content
- re-derive local facts from code when the shared changes do not require it
- treat the update as a full bootstrap
- leave new shared requirements unaddressed in the project law layer
- silently update the project without refreshing or explicitly deferring the shared baseline record

## Difference From Bootstrap
| Aspect | Bootstrap | Update |
|---|---|---|
| Starting point | Code and repository files | Existing localized law layer |
| Direction | Code facts -> law generation | New shared rules -> law merge |
| Local facts | Derived fresh from code | Preserved from existing law |
| Root files | Generated or instantiated | Replaced with new versions |
| Law documents | Generated from scratch | Merged incrementally |
| Tracker | Populated from discovered drift | Reviewed against new rules |

## Responsibility Split

| Layer | Owner | What it does |
| --- | --- | --- |
| Pip package code (CLI, MCP server) | `pipx upgrade harness-kit` | Replaces binaries / Python source |
| Binary schema migration on `.harness/index/meta.db` | Pip package startup hook | Runs migration scripts in lexical order |
| Bundled new shared kit delivery | `pipx upgrade harness-kit` | Updates `scaffold/` inside the package |
| Local governance content merge (`docs/harness/*`, root mirrors, `AGENTS.md`, tracker, baseline) | LLM via this document | Incremental merge, preserves local facts |
| Final integrity verification | User via verify command | Confirms the update produced a valid state |

The split is hard: the LLM does not perform binary DB ALTER directly under any circumstance, and the pip package does not perform governance content merges. Step 2 of the cycle (LLM merge) must run only after step 1 (pipx upgrade) has completed cleanly — running step 2 over a half-migrated database produces silent inconsistencies between the binary state and the law layer.

## Failure Modes

The Completion Checks above describe what a successful update looks like. These are the typical ways the cycle fails in practice and how to recover from each:

- **`pipx upgrade` ran but step 2 was skipped** — new pip code and new schema are in place, but the local law layer still reflects the old shared kit. Result: drift between binaries and governance. Fix: invoke step 2 (the Direct Procedure above).
- **Step 2 ran without `pipx upgrade`** — the LLM has no new shared kit to compare against; the procedure is a no-op or operates on stale assumptions. Fix: run step 1, then re-run step 2.
- **Schema migration failed during `pipx upgrade`** — the MCP server may fail to start. Resolve the underlying cause (DB lock, disk space, schema collision) before continuing. Do not run step 2 over a half-migrated database.
- **Step 2 reverted local facts to generic starter wording** — treated as failure per Failure Conditions above. Roll back the changes and retry, paying attention to the "preserve local facts" constraint.
- **Verification (step 3) flags new shared requirements absent from local law** — step 2 was incomplete. Re-invoke this document focusing on the missing requirements.

## Non-Pip Distribution

When the project does not use the pip-package distribution (the shared kit was delivered through git clone, archive download, or manual copy), step 1 of the cycle changes:

- Replace `pipx upgrade harness-kit` with whatever pulls in the new shared kit (e.g., `git pull`, manual file replacement).
- Schema migrations must be applied by another mechanism documented at the project level. The LLM still does not perform binary DB ALTER directly.

The LLM-driven step 2 (this document's Direct Procedure) and the verification step 3 are unchanged.

## Next Update Trigger
Update this document when:
- the update process proves unclear in real use
- incremental merges produce recurring quality gaps
- the distinction between bootstrap and update needs refinement
