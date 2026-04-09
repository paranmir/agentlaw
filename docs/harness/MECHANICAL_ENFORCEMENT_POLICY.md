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

## Governance Failure Rule
If a mechanically detectable implementation or structure rule repeatedly remains prose-only without justification, treat that as a governance failure.

## Next Update Trigger
Update this document when:
- enforcement criteria change
- enforcement targets expand
- prose-only fallback is being overused
- repeated violations show that stronger executable policy is needed
