---
status: active
scope: repository
seed_reference: true
---

# Harness Update Workflow

## Authority
This document is a contract document. It is the source of truth
for the update procedure propagating upstream harness changes into target projects, shared with target projects via `publish-repo/`,
and its consistency with `HARNESS_UPDATE_TOOL.md` procedural alignment is mechanically enforced
by `rule-harness verify` (`_test_publish_seed_contract_finite_set`).

Governing law: `docs/harness/REPOSITORY_ARTIFACT_RULES.md`. Amendments land
through a plan that updates this file and any dependent law
clause in the same change.

## Purpose

Single reference describing the full update cycle for a Harness-governed project that uses the `rule-harness` pip-package distribution. It explains how the binary infrastructure update (pipx) and the governance content update (`HARNESS_UPDATE_TOOL.md`) fit together.

This document is operational reference, not law. The governing procedure for the LLM-driven step is [`HARNESS_UPDATE_TOOL.md`](../HARNESS_UPDATE_TOOL.md). Authority order applies as usual: constitution > law > approved artifacts > memory.

## When To Update

- A new `rule-harness` version is published upstream and the project should incorporate the changes.
- Time has passed since the last update and the project is suspected to lag behind the shared kit.
- A new shared requirement (law clause, MCP tool, schema migration) is needed for current work.

Do not invoke this workflow for:
- First-time setup → use [`HARNESS_INIT_TOOL.md`](../HARNESS_INIT_TOOL.md).
- Fixing local drift where the agent escaped harness rules → use [`HARNESS_FIX_TOOL.md`](../HARNESS_FIX_TOOL.md).

## Full Update Cycle

The cycle has three steps. Steps 1 and 3 are user-driven (terminal commands). Step 2 is LLM-driven.

### Step 1 — Infrastructure update (terminal)

```bash
pipx upgrade rule-harness
```

This single command:

- replaces the pip package code (CLI, MCP server, schema files, bundled scaffold).
- runs the pip package's startup hook to **automatically apply any pending schema migrations** to `.harness/index/meta.db`. Binary DB ALTER cannot be done safely by the LLM, so the pip package owns it.
- makes the new bundled shared kit available inside the upgraded package's `scaffold/` directory, where the LLM can read it for comparison in step 2.

If the schema migration fails, the pip package surfaces a clear error and the workflow halts. Step 2 must not proceed until step 1 completes cleanly.

### Step 2 — Governance content merge (LLM-driven)

Invoke the LLM with [`HARNESS_UPDATE_TOOL.md`](../HARNESS_UPDATE_TOOL.md). Typical prompt: "Run the harness update procedure in HARNESS_UPDATE_TOOL.md."

The LLM follows the procedure in that document:

1. Reads `references/SHARED_HARNESS_BASELINE.md` to identify the prior baseline.
2. Compares the new shared kit against the baseline.
3. Replaces root mirror files with the new versions.
4. Merges new shared requirements into the local `docs/harness/*` layer without losing local facts.
5. Reviews tracker and enforcement implications.
6. Refreshes `references/SHARED_HARNESS_BASELINE.md` with the new baseline.
7. Updates `AGENTS.md` routing if the read path changed.
8. Verifies that every new shared requirement is traceable in the local law.

The LLM is not allowed to skip these steps or to treat the operation as a fresh bootstrap.

### Step 3 — Verification (terminal)

Run the workspace verification command supplied by the project. Confirm:

- root mirrors match the new shared kit.
- local facts and behavioral oracle content were preserved.
- new shared requirements are present in local law.
- the shared baseline record matches the upgraded package's bundled version.

If verification fails, treat the update as incomplete and resolve the gap before continuing other work.

## Responsibility Split

| Layer | Owner | What it does |
| --- | --- | --- |
| Pip package code (CLI, MCP server) | `pipx upgrade rule-harness` | Replaces binaries / Python source |
| Binary schema migration on `.harness/index/meta.db` | Pip package startup hook | Runs migration scripts in lexical order |
| Bundled new shared kit delivery | `pipx upgrade rule-harness` | Updates `scaffold/` inside the package |
| Local governance content merge | LLM via [`HARNESS_UPDATE_TOOL.md`](../HARNESS_UPDATE_TOOL.md) | Incremental merge, preserves local facts |
| Final integrity verification | User via verify command | Confirms the update produced a valid state |

## Failure Modes

- **`pipx upgrade` ran but step 2 was skipped** — new pip code and new schema are in place, but the local law layer still reflects the old shared kit. Result: drift between binaries and governance. Fix: invoke step 2.
- **Step 2 ran without `pipx upgrade`** — the LLM has no new shared kit to compare against; the procedure is a no-op or operates on stale assumptions. Fix: run step 1, then re-run step 2.
- **Schema migration failed during `pipx upgrade`** — the MCP server may fail to start. Resolve the underlying cause before continuing. Do not run step 2 over a half-migrated database.
- **Step 2 reverted local facts to generic starter wording** — treated as failure per `HARNESS_UPDATE_TOOL.md`. Roll back the changes and retry, paying attention to the "preserve local facts" constraint.
- **Verification (step 3) flags new shared requirements absent from local law** — step 2 was incomplete. Re-invoke `HARNESS_UPDATE_TOOL.md` focusing on the missing requirements.

## Non-Pip Distribution

When the project does not use the pip-package distribution (the shared kit was delivered through git clone, archive download, or manual copy), step 1 changes:

- Replace `pipx upgrade rule-harness` with whatever pulls in the new shared kit (e.g., `git pull`, manual file replacement).
- Schema migrations must be applied by another mechanism documented at the project level. The LLM still does not perform binary DB ALTER directly.

The LLM-driven step 2 (`HARNESS_UPDATE_TOOL.md` procedure) and the verification step 3 are unchanged.

## Related

- [`HARNESS_UPDATE_TOOL.md`](../HARNESS_UPDATE_TOOL.md) — governing procedure for step 2.
- [`HARNESS_INIT_TOOL.md`](../HARNESS_INIT_TOOL.md) — first-time setup; not for updates.
- [`HARNESS_FIX_TOOL.md`](../HARNESS_FIX_TOOL.md) — for fixing local drift, not for upstream updates.
- [`references/SHARED_HARNESS_BASELINE.md`](SHARED_HARNESS_BASELINE.md) — baseline record consulted in step 2.
