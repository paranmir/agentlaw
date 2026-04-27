# Mechanical Enforcement Policy

## Purpose
This document defines when repository rules must be promoted into executable enforcement instead of remaining prose-only guidance.

## Core Rule
Mechanically detectable implementation and structure rules must be promoted into executable enforcement.

They must not remain prose-only unless executable enforcement is currently infeasible.

## Default Enforcement Principle
If the repository can make a forbidden path fail directly, it must prefer that over relying on agent instruction-following alone.

## Enforcement Evaluation Requirement
Every implementation-facing or structure-facing rule must be evaluated for:
- mechanical detectability
- repetition
- cost of failure
- feasibility of enforcement

## Mandatory Promotion Rule
A rule must be promoted into executable enforcement when all of the following are true:
- the rule affects implementation behavior or repository structure
- violation of the rule is mechanically detectable
- violation would create meaningful repair cost, structural drift, or unreliable future work

When these conditions hold, prose-only treatment is not allowed as the default response.

## Allowed Fallback
A rule may remain prose-only only when:
- the rule is not yet mechanically detectable
- enforcement would currently be unreliable
- the rule is still unstable and not yet ready for automation

## Typical Enforcement Targets
Executable enforcement may include:
- lint rules
- CI checks
- structural tests
- document existence checks
- cross-reference checks
- tests that fail forbidden behavior directly
- validation of stable derived facts such as a recorded shared-harness baseline when later update flow depends on it
- package-template or installer-asset sync checks when installable distribution exists
- `.harness/` path checks for governed runtime/index/cache/job purposes
- memory manifest/config shape, authority-label, source-precedence, and status checks when memory formats exist
- leak checks that prevent product-specific, project-specific, or authoring-only facts from entering generalized shared artifacts
- required code-authorship law presence and read-first routing checks
- tests, lint rules, verifier checks, or CI checks for repeated code-authorship violations when they become mechanically detectable
- plan-preflight checks that scan `docs/plans/active/*.md` for the required fields enumerated in `REPOSITORY_ARTIFACT_RULES.md` §"Active Plan Preflight Fields", failing the verifier when a plan omits a field without an explicit "not applicable" note
- publish-gate oracle pairing checks that, when the `pyproject.toml` version differs from a recorded baseline (the bump commit), enumerate `git diff <bump-commit>..HEAD` and fail when modifications appear without paired oracle changes. Two checks per `CODE_AUTHORSHIP_AND_STEWARDSHIP_RULES.md` §"Publish Gate Oracle Pairing": runtime-source pairing (`src/<package>/*.py` modifications must be paired with `tests/` modifications) and harness-content pairing (modifications under `docs/law/`, `docs/contracts/`, `HARNESS_*.md`, `AGENTS.md`, `publish-repo/`, or `src/<package>/scaffold/` must be paired with `tests/` modifications **or** `src/<package>/verify_cmd.py` modifications, the latter counting a new verifier check as the oracle for a new mechanical rule)

## Code Authorship Enforcement
Code-authorship rules are governed by `docs/law/CODE_AUTHORSHIP_AND_STEWARDSHIP_RULES.md`.

Repeated violations of test anchoring, behavioral-oracle use, public-surface coverage, schema migration coverage, or high-risk-code verification must be evaluated for mechanical enforcement.

If a violation can be detected through committed tests, static checks, verifier rules, lint rules, or CI, the repository should prefer that executable enforcement over repeated prose reminders.

## Governance Failure Rule
If a mechanically detectable implementation or structure rule repeatedly remains prose-only without justification, treat that as a governance failure.

## Next Update Trigger
Update this document when:
- enforcement criteria change
- enforcement targets expand
- prose-only fallback is being overused
- repeated violations show that stronger executable policy is needed
