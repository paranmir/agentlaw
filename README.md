# harness-kit

A law-first harness kit for AI coding agents.

`harness-kit` gives a repository a governed starting structure before implementation begins. It is designed for agents that can read and rewrite repository files directly, and it is built to work for both brand-new repositories and already-existing codebases.

## What This Is

This repo is a reusable starter kit for:
- Codex
- Claude Code
- Gemini CLI
- other coding agents with direct repository access

It is not:
- an application framework
- a language-specific scaffold
- a build system
- a test runner

Its job is narrower:
- establish governance first
- keep entry guidance short
- move durable meaning into structured law documents
- preserve starter protections during specialization
- promote repeated, detectable violations into executable enforcement

## Repository Structure

```text
.
├─ HARNESS_CONSTITUTION.md
├─ AGENTS.md
├─ BOOTSTRAP_PROJECT_PROMPT.md
├─ STARTER_SPECIALIZATION_RULES.md
├─ PROBLEM_ANALYSIS_AND_RULE_ADDITION.md
├─ MECHANICAL_ENFORCEMENT_POLICY.md
├─ docs/
│  └─ harness/
│     ├─ HARNESS_SCOPE.md
│     ├─ INPUT_OUTPUT_CONTRACT.md
│     ├─ ORACLE_AND_JUDGMENT.md
│     ├─ FAILURE_TAXONOMY.md
│     └─ REPOSITORY_ARTIFACT_RULES.md
├─ plans/
│  ├─ active/
│  ├─ completed/
│  └─ tech-debt-tracker.md
└─ references/
   └─ README.md
```

## What Each Part Does

### Constitution
- `HARNESS_CONSTITUTION.md`
- top-level governance and stable structure

### Root Control Layer
- `BOOTSTRAP_PROJECT_PROMPT.md`
- `STARTER_SPECIALIZATION_RULES.md`
- `PROBLEM_ANALYSIS_AND_RULE_ADDITION.md`
- `MECHANICAL_ENFORCEMENT_POLICY.md`

These documents help an agent:
- initialize the harness
- specialize it into a real project
- analyze governance problems
- decide when prose must become executable enforcement

### Law Layer
- `docs/harness/*`

This is where the real project meaning belongs:
- scope
- contract
- judgment
- failure handling
- artifact structure

### Operational Artifacts
- `plans/*`
- `references/*`

These support the law layer. They do not replace it.

### Execution Entry
- `AGENTS.md`

This stays short and routes the agent into the right files.

## Core Principles

- Law-first before implementation
- Short `AGENTS.md`, never a rule encyclopedia
- Shared starter protections survive project specialization
- Existing projects must be rewritten from real code and runtime facts, not vague starter prose
- Repeated, mechanically detectable violations should become executable enforcement

## How To Use

### 1. Put `harness-kit` at the root of the target repository

You can:
- copy these files into the repository
- extract the distributed zip into the repository
- or otherwise place this structure at repository root

The target repository should end up with the files and directories shown above.

### 2. Start with the bootstrap entry point

Read:
- `BOOTSTRAP_PROJECT_PROMPT.md`

This is the first tool-facing entry point. It tells the agent how to classify the repository and how to initialize or rebuild the harness correctly.

### 3. Read the governing order

Then read in this order:
1. `HARNESS_CONSTITUTION.md`
2. the root control layer as needed
3. `docs/harness/*`
4. `AGENTS.md`

If plans or references already exist, read them only when relevant.

### 4. Classify the target repository

The bootstrap is designed for three modes:

- `empty`
  - almost no product files exist yet
- `near-empty`
  - some structure exists, but product meaning is still mostly undefined
- `existing`
  - code, runtime artifacts, or meaningful project behavior already exist

This classification matters. The harness should not treat all repositories as greenfield.

### 5. Run the bootstrap correctly

#### For empty or near-empty repositories
The agent should:
- instantiate the starter law layer
- define the initial first-release boundary
- keep unknowns explicit
- avoid inventing extra structure without need

#### For existing repositories
The agent should:
- preserve starter structure and protections
- treat current code and file structure as the authoritative product baseline
- rewrite `docs/harness/*` from repository facts
- replace generic starter wording with concrete local meaning
- make important code/UI/doc mismatches explicit
- use `plans/tech-debt-tracker.md` when repeated, material, or enforcement-relevant discrepancies already exist

### 6. Do not implement too early

Implementation should start only after the law layer is materially ready.

At minimum, the repository should have readable law for:
- scope
- input/output contract
- judgment
- failure handling
- execution flow and regression expectations

## How To Use The Root Control Documents

### `BOOTSTRAP_PROJECT_PROMPT.md`
Use this when:
- initializing a new repository
- rebuilding harness docs in an existing repository
- discarding weak earlier harness output and regenerating it from the current starter

### `STARTER_SPECIALIZATION_RULES.md`
Use this when:
- converting starter law into project-specific law
- checking whether starter protections were preserved
- reviewing whether the result is too generic for an existing codebase

### `PROBLEM_ANALYSIS_AND_RULE_ADDITION.md`
Use this when:
- the agent escaped the harness
- a repeated governance mistake appears
- you need a step-by-step tool for classifying the problem
- you need to decide whether the issue belongs in law, tracker, `AGENTS.md`, or enforcement

Typical use:
1. record the problem
2. classify the gap
3. check existing rules first
4. choose the owning layer
5. apply the smallest sufficient correction
6. verify references and follow-up

### `MECHANICAL_ENFORCEMENT_POLICY.md`
Use this when:
- a repeated rule violation is mechanically detectable
- prose-only guidance is no longer enough
- a repository constraint should become lint, CI, structure checks, or tests

## Expected Result Of A Good Bootstrap

For any repository, a good bootstrap should produce:
- a short `AGENTS.md`
- a readable law layer under `docs/harness/*`
- explicit blockers where meaning is still missing
- no premature implementation

For an existing repository, a good bootstrap should also produce:
- concrete local law text
- explicit important mismatches
- non-empty tracker entries when real repeated or materially important drift already exists

## Beta Status

`harness-kit` is still beta.

It is currently strongest for:
- repository initialization
- governance scaffolding
- law-first project setup
- rebuilding structure around an already-existing codebase

It is still evolving through recursive improvement based on real agent failures and bootstrap results.
