# Oracle And Judgment

## Purpose
This document defines how work in the current project is judged as acceptable, incomplete, or incorrect.

## Known
- Early judgment is based on harness readiness before product correctness.
- Implementation is blocked until the law layer contains enough criteria to govern the work.

## Unknown
- product-specific acceptance thresholds
- domain-specific correctness rules
- regression boundaries

## Assumptions
- The current project is still establishing its first governed baseline.
- Structural document completeness is always required, even after the project gains a behavioral oracle.

## Open Questions
- What will count as success for the product or system itself?
- What evidence will later prove the implementation is correct?
- What regression boundaries matter first?

## Blocked Until Clarified
- implementation that claims correctness without an explicit oracle
- pass or fail decisions that rely on undocumented judgment rules

## Structural Oracle
The structural oracle checks whether the governed document system is sound:
- the minimum law-layer files exist
- `Known`, `Unknown`, and `Assumptions` are separated
- implementation blockers are explicit
- open questions are visible

The structural oracle remains active throughout the project and does not disappear when product behavior becomes clearer.

## Behavioral Oracle
The behavioral oracle checks whether the project's first-release product or system behavior is correct.

Before the first-release boundary becomes materially known, this oracle may remain partial.

Once the project can describe what the first release is supposed to do, the behavioral oracle must be written in terms of concrete product or system behavior rather than document existence alone.

Document existence alone is never a sufficient behavioral oracle.

### Executable-Code Oracle Requirement
Once the project ships executable code or a runtime (any source that can be installed and run — for example a pip-buildable `src/` tree, a bundled CLI, a server, or an MCP runtime), the behavioral oracle must exist as **committed automated tests** that another agent or CI can run from a fresh clone without session-specific context.

Ad-hoc smoke scripts — tests typed into a terminal or a scratch file during a single session — are evidence of in-session work, not oracles. They must be converted into committed tests before the next structural change lands. A repository that ships executable code without committed behavioral tests is missing the behavioral oracle the rest of this document requires.

This rule applies equally to the shared kit and to any project that specializes from it: once real code ships, automated behavioral verification ships with it.

### Representative Smoke Test Boundary
Representative smoke tests are useful but not sufficient as the behavioral oracle for shipped features.

A smoke test may prove that:
- an executable entry point starts
- a transport or integration path can complete one valid round trip
- a freshly initialized system is not obviously broken

A smoke test must not be treated as proof that every shipped feature, tool, command, status path, or error path is correct.

When a runtime exposes multiple commands, tools, routes, protocols, or user-visible operations, the verification baseline must include either:
- direct behavioral tests for each shipped feature's contract, or
- an explicit coverage matrix that maps each shipped feature to the test that exercises its valid path, invalid path when applicable, state change, persistence or reload behavior, and authority or boundary checks.

It is acceptable for one test to cover multiple features only when the assertion for each feature is explicit enough that a failure identifies which contract broke. A single representative example with broad claims about the rest of the surface is incomplete.

### Risk-Based Contract Coverage
Behavioral verification should use risk-based contract coverage rather than exhaustive combination testing.

For each shipped feature, command, tool, route, protocol surface, or user-visible operation, the verification plan must identify the relevant contract evidence:
- valid path behavior
- invalid or unsupported path behavior when such a path exists
- state-change evidence when the feature mutates files, databases, runtime state, or external outputs
- persistence, reload, repeat-run, or round-trip evidence when state is stored or later retrieved
- boundary, authority, or safety checks when the feature can cross governance, scope, filesystem, network, permission, or safety boundaries

Boundary-value analysis should be added where the feature has meaningful numeric, enum, pagination, size, time, status, collision, path, or concurrency boundaries.

The project should not require exhaustive Cartesian-product testing unless the product's risk profile or a prior failure shows that those combinations are load-bearing. Prefer small, contract-revealing tests that fail close to the broken behavior.

If a test intentionally covers multiple contracts, its assertions must make the covered contracts visible. A broad end-to-end scenario without contract-specific assertions is smoke evidence, not full behavioral coverage.

When the shipped surface grows large enough that coverage is hard to see from test names alone, add or update a structured coverage matrix under an approved repository artifact path such as `docs/references/*` or `docs/plans/*`. The matrix is supporting evidence, not a replacement for executable tests.

### Code Authorship Oracle
Code work is governed by `docs/law/CODE_AUTHORSHIP_AND_STEWARDSHIP_RULES.md`.

Tests for code work must identify the behavioral oracle before asserting expected behavior. The oracle priority and test-anchored development rules live in that document.

When a code-authorship rule and a product-specific behavioral oracle both apply, use the product-specific oracle to define the expected behavior and the code-authorship rules to define the required testing, maintainability, dependency, and verification discipline.

### Reviewable Verification Context
For non-trivial work, the verification oracle must be documented before completion. The oracle may live in tests, an active plan, or a design/reference document, but it must identify what regression, contract, or invariant the verification protects.

Test comments should explain the protected regression or contract only when that oracle is not obvious from the test name and assertions.

### Affected Surface Preflight
Before non-trivial implementation, refactoring, public-contract documentation, package, runtime, configuration, or cross-component work begins, the agent must identify the affected surfaces.

The preflight gate applies when work changes behavior, public contract, verification expectations, runtime state, package or install behavior, user or agent configuration, persistence, schema, cross-platform behavior, or cross-component structure.

Trivial typo, formatting, or wording cleanup that does not change behavior, public contract, verification expectations, or operational meaning may proceed with the smallest relevant verification.

Affected surfaces include, when applicable:
- runtime behavior
- persistence, schema, migration, repair, or recovery
- CLI, MCP, API, protocol, or public command behavior
- package, install, scaffold, release, upgrade, or existing-target behavior
- agent, user, host, or project configuration
- public contract documentation, including user-visible commands, promises, support status, return shapes, install paths, and completion criteria
- cross-platform path, process, quoting, filesystem, or environment behavior
- model, network, dependency, cache, or external service behavior

Verification impact must be considered separately:
- tests
- coverage matrices
- verification profiles
- release smoke checks
- documented manual checks when automation is not currently feasible

For every affected surface, the agent must identify the oracle that will prove the change correct. If no reliable oracle exists, the first task is to create or select one.

For high-risk work, deferring the oracle requires explicit user approval or a recorded blocking reason. Convenience, speed, or broad test-suite success is not a valid reason to defer the oracle.

Source-tree tests are not sufficient when package, install, release, upgrade, existing-target, or console-script behavior is affected. A package-install or upgrade smoke oracle is required or must be explicitly deferred under the high-risk deferral rule.

When CLI, MCP, API, README, reference, or help text is affected, the public contract sources must agree before the work is complete.

When persistence, schema, repair, runtime index, or recovery behavior is affected, the oracle must include reload, repair, migration, or recovery evidence as applicable.

When model, network, dependency, or cache behavior is affected, the oracle must cover absent, degraded, or unavailable dependency behavior unless the change explicitly narrows support.

### Component-Scoped Verification
Verification should be scoped to the changed component before it is expanded to the whole repository.

A change that only touches a narrow component should first run the smallest committed tests that directly exercise that component's contract. Running the entire suite is not a substitute for choosing the relevant targeted tests; it can hide which contract was meant to be protected.

The verification scope must expand when a change crosses component boundaries, changes shared contracts, changes schema or migration behavior, changes package/scaffold synchronization, changes CLI entry points, changes law or root control documents, or alters behavior represented in a coverage matrix.

Before declaring a multi-file or user-facing change complete, the agent must run the repository's current whole-repo verification baseline unless the user explicitly asks to defer it. The final verification baseline may be a full automated test suite, a harness integrity check, or both, depending on the changed surface.

Slow, network-dependent, model-backed, large-data, or environment-sensitive tests should not be part of the default tight loop unless the changed component requires them. They should live behind an explicit profile or command, and the default suite must still prove graceful degradation when those dependencies are absent.

## Installer Judgment
Installer correctness, when installer work is in scope, is structural and procedural.

An installer can prove that expected files, directories, manifests, or metadata artifacts were placed according to contract.

An installer does not prove that localized project law is semantically correct.

## Memory Judgment
Memory correctness is judged by whether memory remains useful derived continuity without becoming authority.

At minimum, memory judgment must check:

- repository files outrank memory records
- memory records are scoped
- source or provenance is visible when durable memory influences context
- stale or conflicting memory is not presented as current truth
- conflicts follow `authority > scope > recency`
- conflicting memory changes status before ordinary retrieval
- context assembly stays compact rather than becoming broad prompt stuffing
- memory may surface recursive-improvement candidates but does not promote them directly into law or shared artifacts

## Oracle Emphasis Rule
Early work may rely more heavily on the structural oracle.

Once the first-release boundary is materially known, judgment must use both:
- the structural oracle for governance integrity
- the behavioral oracle for product or system correctness

## Instance Oracle Preservation Rule
When this shared harness kit is specialized into a project-specific law set, behavioral oracle content should become more concrete and product-facing.

That specialization must extend the oracle system rather than collapsing it.

Project-specific oracle writing must preserve, unless a higher-order governing rule explicitly changes them:
- the continued existence of the structural oracle
- the requirement that behavioral oracle content be concrete once the first-release boundary is materially known
- the rule that document existence alone is not a sufficient behavioral oracle
- the expectation that both oracle layers remain usable for later review and implementation judgment

These are mandatory carry-forward protections for project instances and must remain explicitly readable in the resulting project law document rather than only implied through local prose.

## Acceptance Criteria
A change is acceptable when:
- it preserves the governing hierarchy
- it makes missing information explicit rather than hiding it
- it does not bypass the documentation gate
- it does not treat assumptions as facts
- it keeps both structural and behavioral judgment explicit enough for another agent to follow

## Documentation Gate Judgment
Implementation is documentation-incomplete until the law layer defines:
- scope boundaries
- contract expectations
- judgment criteria
- failure classification
- execution flow and regression expectations

## Implementation Readiness Rule
Implementation may begin when the project has:
- a usable structural oracle that another agent could follow with reasonable consistency
- a usable behavioral oracle for the current first-release boundary

Open questions may remain if they do not prevent correct judgment of the current first-release boundary.

Implementation must remain blocked if correctness still depends on undocumented assumptions or if either oracle is too weak to judge the first-release boundary safely.

## Change Judgment
When a request arrives, judge it in this order:
1. Does it change governance meaning?
2. Does it require law-document updates?
3. Is implementation still blocked?

## Consistency Judgment
If documents conflict, restore consistency at the layer that owns the meaning before proceeding.

## Regression Expectations
Regression includes:
- implementation starting before the documentation gate is satisfied
- assumptions being treated as facts
- open questions disappearing without resolution
- lower-order documents overriding higher-order documents
- memory records or installer metadata being treated as higher authority than law or approved repository artifacts
- recursive-improvement candidates being promoted without reviewed law, plan, reference, or shared-artifact changes

## Evidence Of Completion
Initial governance completion requires:
- the minimum law-layer set exists
- unresolved items are visible
- implementation blockers are accurately stated

Once the first-release boundary is materially known, completion evidence must also include a product-facing oracle that another agent could apply without inferring missing behavior.

## Review Checks
- Are judgment rules explicit enough for another agent to follow?
- Are blockers still real or already resolved?
- Does the project now need more specific oracle rules?
- Is the structural oracle still intact?
- If the first-release boundary is already known, is the behavioral oracle explicit enough to judge product or system behavior?

## Minimum Verification Baseline
At minimum, verification should cover:
- one valid core flow inside the first-release boundary
- one invalid or unsupported input path when such a path exists
- one state-change check that proves the main outcome really changed
- one persistence, reload, repeat-run, or equivalent continuity check when that behavior matters
- one check that out-of-scope behavior was not silently introduced

When the project has multiple distinct features or entry points, the minimum verification baseline should cover each feature — not just the primary one. A verification baseline that omits a shipped feature is incomplete.

The minimum verification baseline should be written so that another agent can test concrete behavior, not only document completeness.

Smoke or transport tests may count toward this baseline only for the specific entry point, transport, or integration behavior they actually assert. They do not substitute for feature-level behavioral tests.

## Regression Strategy
Regression expectations should identify:
- the smallest behavior set that defines the first release
- the behaviors most likely to break after refactoring or extension
- the stored, repeated, or cross-step outcomes that must remain stable
- the points where a change should force the verification baseline to expand
- the shipped features whose contract coverage is represented by tests or a coverage matrix

For memory behavior, regression expectations should also identify:
- conflict-precedence checks proving repository files win over memory
- wrong-scope memory checks
- over-retrieval or context-pollution checks
- auditability checks for memory-derived context

## Structured Expansion Judgment
The project should judge its documentation structure as insufficient when:
- important decisions no longer fit cleanly inside the current law layer and short execution map
- complex work proceeds without a versioned plan
- important references are repeatedly needed but live only outside the repository
- the same review rule recurs often enough that prose-only enforcement becomes unreliable

When one or more of these conditions appears, the correct response is to expand the repository structure rather than overloading `AGENTS.md`.

## Next Update Trigger
Update this document when:
- product-specific judgment becomes clearer
- regression expectations change
- implementation readiness changes
