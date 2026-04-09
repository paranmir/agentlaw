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

## Required Inputs
Before using this document, read:
- `HARNESS_CONSTITUTION.md`
- `references/SHARED_HARNESS_BASELINE.md` when it exists
- the current localized law layer in `docs/harness/*`
- the new shared-kit versions of the root and law documents
- `AGENTS.md` when routing may need to change

## Direct Procedure
Use this document as a direct procedure even if no prompt block is reused.

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
- Prefer the recorded shared-harness baseline in `references/SHARED_HARNESS_BASELINE.md` when identifying the previous shared version.

Procedure:

1. Identify what changed in the shared kit.
   - First read `references/SHARED_HARNESS_BASELINE.md` if it exists.
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
   - Update `references/SHARED_HARNESS_BASELINE.md` to record the shared source repository and the commit or tag now reflected in the project.
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
- refreshed `references/SHARED_HARNESS_BASELINE.md`
- updated `AGENTS.md` routing if the read-first order changed

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

## Next Update Trigger
Update this document when:
- the update process proves unclear in real use
- incremental merges produce recurring quality gaps
- the distinction between bootstrap and update needs refinement
