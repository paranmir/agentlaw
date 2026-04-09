# Update Harness Tool

## Purpose
Use this document when the shared harness kit has been updated and a target project must incorporate the new or changed rules into its existing localized law layer.

This document is a shared update entry point for any coding agent. It is not itself a law document. Its role is to merge upstream harness changes into an already-bootstrapped project without losing local facts.

## When To Use This Document
Use this document when:
- the shared harness kit received new rules, requirements, or structural changes since the project was last bootstrapped or updated
- the project already has a localized law layer from a prior bootstrap
- the goal is incremental incorporation, not a full rebuild

Do not use this document when:
- the project has never been bootstrapped — use `BOOTSTRAP_PROJECT_TOOL.md` instead
- the goal is to discard and recreate the law layer from scratch — use `BOOTSTRAP_PROJECT_TOOL.md` instead
- a specific governance problem needs analysis — use `PROBLEM_ANALYSIS_AND_RULE_ADDITION_TOOL.md` instead

## Update Prompt
Use the following prompt, adapted only as needed for the target repository path or user intent:

```text
You are updating the harness governance in this repository to incorporate changes from a newer version of the shared harness kit.

Your job is to merge new shared requirements into the existing localized law layer without losing project-specific facts.

Follow these rules:
- The existing localized law layer is the starting point, not the new shared starter templates.
- New shared requirements must be incorporated, but existing local facts, local mismatches, local tracker entries, and local behavioral oracle content must be preserved.
- Do not re-derive local facts from code unless a new shared requirement specifically demands new code inspection.
- Do not replace localized law documents with fresh starter templates.
- Do not treat this as a bootstrap. The project already has project-specific law.

Procedure:

1. Identify what changed in the shared kit.
   - Compare the new shared kit files against the versions the project was last bootstrapped or updated from.
   - If the previous shared kit version is not available for comparison, read the new shared kit files and compare their requirements against the current project law layer to identify gaps.
   - Categorize each change as: new requirement, strengthened requirement, relaxed requirement, structural change, or wording-only clarification.

2. Replace root-level mirror files.
   - `HARNESS_CONSTITUTION.md`: replace with the new version.
   - Root control documents (`BOOTSTRAP_PROJECT_TOOL.md`, `PROBLEM_ANALYSIS_AND_RULE_ADDITION_TOOL.md`, `UPDATE_HARNESS_TOOL.md`): replace with the new versions.
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

5. Update `AGENTS.md` if needed.
   - If the read-first order changed (e.g., a new root control document was added), update the routing.
   - Keep `AGENTS.md` short and routing-only.

6. Verify completeness.
   - Every new shared requirement should be traceable in the updated project law layer.
   - No local fact should have been lost during the merge.
   - No localized law document should have reverted to generic starter wording.
   - The update should not have introduced any project-type-specific language from the shared kit that does not apply to this project.
```

## Expected Output Of A Good Update
A correct update should produce:
- updated root-level mirror files matching the new shared kit
- localized law documents that incorporate new shared requirements while preserving all prior local facts
- reviewed tracker entries with promotions or new items when applicable
- updated `AGENTS.md` routing if the read-first order changed

It should not:
- replace localized law documents with fresh starter templates
- lose project-specific facts, mismatches, or behavioral oracle content
- re-derive local facts from code when the shared changes do not require it
- treat the update as a full bootstrap
- leave new shared requirements unaddressed in the project law layer

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
