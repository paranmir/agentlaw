# harness-kit

A law-first governance kit for AI coding agents — install governance structure before writing code.

`harness-kit` gives any repository a governed starting structure that AI coding agents can read, follow, and maintain. It works for brand-new repositories and already-existing codebases alike. Drop it in, run the bootstrap, and the agent knows what the rules are before it writes a single line.

## Quick Start

### 1. Place harness-kit at the root of your target repository

Copy or extract these files into the repository root. The target repository should end up with the structure shown in [Repository Structure](#repository-structure) below.

### 2. Point the agent at the bootstrap entry

Tell the agent to read `HARNESS_INIT_TOOL.md`. This is the first tool-facing entry point — it classifies the repository and runs the correct initialization.

### 3. Let the agent bootstrap

The agent will:
- classify the repository as `empty`, `near-empty`, or `existing`
- generate a localized law layer under `docs/harness/*`
- produce a short `AGENTS.md` as the execution-entry map
- leave explicit blockers where meaning is still missing

After bootstrap, the law layer is ready and implementation can begin under governance.

---

## Abstract

### The Problem

AI coding agents can generate large volumes of code quickly, but without structure they drift. A massive `AGENTS.md` file becomes stale. Rules that live only in prose get ignored. Architecture decisions made in chat threads are invisible to the next agent session. The codebase accumulates entropy faster than humans can review it.

### The Origin

`harness-kit` implements the principles from OpenAI's [harness engineering technical article](https://openai.com/index/harness-engineering/), which describes the approach they developed while building a product entirely with Codex agents — zero hand-written code. The core insight: when agents do the coding, the engineer's job shifts from writing code to **designing the environment, expressing intent clearly, and building feedback loops** so agents can work reliably.

Key principles from that approach:
- **AGENTS.md is a map, not an encyclopedia** — short entry point that routes to deeper docs
- **The repository is the record system** — structured docs, versioned plans, quality grades, all in-repo
- **Progressive disclosure** — agents start from a small stable entry point, then drill deeper as needed
- **Mechanical enforcement** — repeated rules graduate from prose to linters, CI, and structural tests
- **Garbage collection** — regular cleanup of drift, not quarterly debt sprints

### What harness-kit Does

`harness-kit` packages these principles into a reusable starter kit that any project can adopt immediately:

| Principle | harness-kit mechanism |
|---|---|
| Map, not encyclopedia | Short `AGENTS.md` routes to `docs/harness/*` |
| Repository as record system | Constitution → law layer → operational artifacts hierarchy |
| Progressive disclosure | Read-order from constitution down to plans/references |
| Mechanical enforcement | `MECHANICAL_ENFORCEMENT_POLICY.md` + promotion rules |
| Garbage collection | `HARNESS_FIX_TOOL.md` + tech-debt tracker |
| Protect invariants during growth | `STARTER_SPECIALIZATION_RULES.md` + carry-forward rules |

It adds two things the original article leaves to each team:
1. **A portable starter structure** — so you don't have to design the governance layout from scratch
2. **Root control tools** — so the agent can initialize, update, and repair governance itself

---

## Repository Structure

```text
.
├─ HARNESS_CONSTITUTION.md
├─ AGENTS.md
├─ HARNESS_INIT_TOOL.md
├─ HARNESS_UPDATE_TOOL.md
├─ HARNESS_FIX_TOOL.md
├─ docs/
│  └─ harness/
│     ├─ HARNESS_SCOPE.md
│     ├─ INPUT_OUTPUT_CONTRACT.md
│     ├─ ORACLE_AND_JUDGMENT.md
│     ├─ FAILURE_TAXONOMY.md
│     ├─ REPOSITORY_ARTIFACT_RULES.md
│     ├─ STARTER_SPECIALIZATION_RULES.md
│     └─ MECHANICAL_ENFORCEMENT_POLICY.md
├─ plans/
│  ├─ active/
│  ├─ completed/
│  └─ tech-debt-tracker.md
└─ references/
   └─ README.md
```

## What Each Layer Does

### Constitution

`HARNESS_CONSTITUTION.md` — the top-level governing document. It fixes stable rules for document structure, authority hierarchy, documentation gates, and change handling. It does not contain project-specific detail.

### Root Control Tools

Three tool-facing documents that help agents manage the harness itself:

| Tool | When to use |
|---|---|
| `HARNESS_INIT_TOOL.md` | Initializing a new repository, or rebuilding harness docs in an existing one |
| `HARNESS_UPDATE_TOOL.md` | Incorporating upstream harness changes while preserving existing local facts |
| `HARNESS_FIX_TOOL.md` | Analyzing governance problems, classifying gaps, placing corrections at the right layer |

### Law Layer

`docs/harness/*` — where the real project meaning belongs. This is the source of truth for:

| Document | Governs |
|---|---|
| `HARNESS_SCOPE` | Scope, non-scope, and target boundaries |
| `INPUT_OUTPUT_CONTRACT` | Required inputs, outputs, and interface expectations |
| `ORACLE_AND_JUDGMENT` | Judgment logic, oracle rules, and acceptance criteria |
| `FAILURE_TAXONOMY` | Failure classes, error interpretation, and handling boundaries |
| `REPOSITORY_ARTIFACT_RULES` | Artifact naming, structure, and expansion rules |
| `STARTER_SPECIALIZATION_RULES` | How starter protections survive project-specific rewriting |
| `MECHANICAL_ENFORCEMENT_POLICY` | When and how prose rules graduate to executable enforcement |

### Operational Artifacts

- `plans/*` — versioned execution plans (active, completed, tech-debt tracker)
- `references/*` — searchable repository-local context

These support the law layer. They do not replace it.

### Execution Entry

`AGENTS.md` — stays short and routes the agent into the right files. Never a rule encyclopedia.

## Governing Hierarchy

The authority order, from highest to lowest:

1. `HARNESS_CONSTITUTION.md`
2. Root control tools (`HARNESS_INIT_TOOL`, `HARNESS_UPDATE_TOOL`, `HARNESS_FIX_TOOL`)
3. `docs/harness/*` (law layer)
4. `plans/*`, `references/*` (operational artifacts)
5. `AGENTS.md` (execution entry)

Higher layers override lower layers. Project-specific criteria live in the law layer, not in the constitution or execution entry.

## Three Modes of Use

### Bootstrap (`HARNESS_INIT_TOOL.md`)

For first-time setup. The agent classifies the repository and generates the initial law layer.

**Empty / near-empty repositories:**
- Instantiate starter law layer
- Define initial first-release boundary
- Keep unknowns explicit
- Avoid inventing structure without need

**Existing repositories:**
- Preserve starter structure and protections
- Rewrite `docs/harness/*` from actual repository code and runtime facts
- Replace generic starter wording with concrete local meaning
- Document important code/interface/doc mismatches
- Use `plans/tech-debt-tracker.md` for repeated or material discrepancies

### Update (`HARNESS_UPDATE_TOOL.md`)

For incorporating upstream harness changes into an already-bootstrapped project. Merges new shared requirements while preserving local facts — no full rebuild needed.

### Fix (`HARNESS_FIX_TOOL.md`)

For when the agent escapes the harness or a governance gap appears. Step-by-step flow:
1. Record the problem
2. Classify the gap
3. Check existing rules
4. Choose the owning layer
5. Apply the smallest sufficient correction
6. Verify references and follow-up

## Core Principles

- **Law before implementation** — document scope, contract, judgment, and failure handling before coding starts
- **Short AGENTS.md** — entry routing only, never a rule store
- **Starter protections survive specialization** — project-specific rewriting adds local meaning without deleting structural safeguards
- **Existing projects rewrite from facts** — concrete code and runtime details replace generic starter prose
- **Prose graduates to enforcement** — repeated mechanically detectable violations become linters, CI checks, or structural tests

## Expected Result of a Good Bootstrap

For any repository:
- A short `AGENTS.md`
- A readable law layer under `docs/harness/*`
- Explicit blockers where meaning is still missing
- No premature implementation

For existing repositories, additionally:
- Concrete local law text (not generic starter wording)
- Explicit important mismatches
- Non-empty tracker entries when real repeated or material drift already exists

## Compatible Agents

`harness-kit` is designed for any coding agent with direct repository access:
- Codex
- Claude Code
- Gemini CLI
- other agents that can read and rewrite files

It is not a framework, scaffold, build system, or test runner. It governs — the agent implements.

## Beta Status

`harness-kit` is still in beta. It is currently strongest for:
- Repository initialization
- Governance scaffolding
- Law-first project setup
- Rebuilding structure around existing codebases

It evolves through recursive improvement based on real agent failures and bootstrap results.
