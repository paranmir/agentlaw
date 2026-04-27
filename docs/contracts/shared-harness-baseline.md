# Shared Harness Baseline

## Authority
This document is a contract document. It is the source of truth
for the distribution contract between rule-harness and target projects, shared with target projects via `publish-repo/`,
and its consistency with the `publish-repo/` tree and target project layout is mechanically enforced
by `rule-harness verify` (`_test_scaffold_drift` + `_test_publish_seed_contract_finite_set`).

Governing law: `docs/harness/REPOSITORY_ARTIFACT_RULES.md`. Amendments land
through a plan that updates this file and any dependent law
clause in the same change.

## Purpose
This local workspace is itself a bootstrapped project instance of the shared Harness kit.

This file records the shared-kit baseline currently reflected in the workspace.

## Baseline Record
- Shared source repository: `https://github.com/paranmir/rule-harness.git`
- Baseline commit SHA: `2d7b12d`
- Baseline tag: `unknown`
- Baseline recorded on: `2026-04-27`
- Recorded by: `readme-polish-and-publish-repo-sync plan`
- Notes:
  - Root constitution and root control documents are mirrors of `publish-repo/*`.
  - The baseline commit was read from `publish-repo/.git`.
  - This update includes: failure taxonomy expanded to 7 classes, Restore/Save Request Protocols added to REPOSITORY_ARTIFACT_RULES, genericization gap detection added to FIX/UPDATE tools.
