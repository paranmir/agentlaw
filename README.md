# harness-kit

A law-first harness kit for AI coding agents.

`harness-kit` helps a repository establish governance before implementation begins. It gives coding agents a structured starting point for bootstrap, law documents, specialization, problem analysis, and enforcement promotion.

## Who This Is For

This kit is designed for:
- Codex
- Claude Code
- Gemini CLI
- other coding agents that can read repository files directly

## What This Repo Gives You

Root control layer:
- `HARNESS_CONSTITUTION.md`
- `AGENTS.md`
- `BOOTSTRAP_PROJECT_PROMPT.md`
- `STARTER_SPECIALIZATION_RULES.md`
- `PROBLEM_ANALYSIS_AND_RULE_ADDITION.md`
- `MECHANICAL_ENFORCEMENT_POLICY.md`

Law layer:
- `docs/harness/HARNESS_SCOPE.md`
- `docs/harness/INPUT_OUTPUT_CONTRACT.md`
- `docs/harness/ORACLE_AND_JUDGMENT.md`
- `docs/harness/FAILURE_TAXONOMY.md`
- `docs/harness/REPOSITORY_ARTIFACT_RULES.md`

Operational artifacts:
- `plans/active/`
- `plans/completed/`
- `plans/tech-debt-tracker.md`
- `references/README.md`

## What It Does

This kit is built to help agents:
- bootstrap a new repository before writing product code
- establish a law-first document structure
- preserve starter protections during project-specific specialization
- analyze current governance problems before adding new rules
- promote repeated, mechanically detectable violations into executable enforcement

## Repository Modes

`harness-kit` is designed to work with:
- empty repositories
- near-empty repositories
- existing repositories with meaningful code or runtime artifacts

It does not assume every project starts from zero.

## How To Use

1. Put this kit at the root of your target repository.
2. Start with `BOOTSTRAP_PROJECT_PROMPT.md`.
3. Read `HARNESS_CONSTITUTION.md`.
4. Follow `AGENTS.md`.
5. Fill `docs/harness/*` from the actual repository state.
6. Begin implementation only after the law layer is materially ready.

If the repository already has code:
- treat current code and file structure as the authoritative product source
- treat old design prose as reference-only unless the law layer explicitly promotes something out of it

## Core Principles

- Law-first before implementation
- Short `AGENTS.md`, never a rule encyclopedia
- Structured docs instead of one giant instruction file
- Specialization is overlay, not replacement
- Repeated detectable violations should become executable enforcement
- If a forbidden path can be made to fail directly, prose-only guidance is not enough

## Current Status

This repo is currently **beta**.

It is best suited for:
- repository initialization
- harness setup
- governance scaffolding
- early project structuring
- rebuilding governance around an already-existing codebase

## Notes

This repository is a starter kit, not a finished application framework.

It does not prescribe:
- a programming language
- a frontend or backend stack
- a package manager
- a test framework
- a CI provider

Those belong to the target project once the law layer is established.
