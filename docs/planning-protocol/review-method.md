# Review Method

## Purpose

Turn a draft plan into a safer revised plan by reviewing it from targeted expert
perspectives.

Personas improve the plan. They do not execute the task, replace tests, or
create an endless review loop.

## Workflow

Use this sequence when `task-classification.md` says planning is required:

1. Classify the request and identify task class(es).
2. Determine importance/risk by checking against
   `task-classification.md` § Non-Trivial Triggers. Cite each matched
   trigger.
3. Draft the plan using `plan-template.md`. For non-trivial work, self-mark
   the applicable conditional domains in the Domain Coverage section.
4. Select the persona deck per § Selection Rule below.
5. Run **one persona review at a time, sequentially**. Each persona is
   invoked in its own turn so that lens injection from one persona does not
   cross-contaminate the next persona's review. Within a single turn, the
   persona produces concrete findings against the plan as written. The
   **Trigger Coverage Verifier persona always runs first**; it validates
   the trigger marking and the Domain Coverage self-mark against the plan
   content.
6. Consolidate findings.
7. Revise the plan. **Section-based re-review**: when revisions affect a
   specific subset of plan sections, re-invoke only the personas whose
   primary or optional sections (per `persona-section-map.md`) include the
   revised sections. Personas whose sections were not revised do not need
   re-invocation.
8. Execute the revised plan or ask for user approval when required.

For trivial plan-required work, only step 5's Trigger Coverage Verifier
runs. If it confirms the trivial classification, no further personas
execute. If it finds a missed trigger, the plan is reclassified as
non-trivial and the workflow restarts from step 4.

Do not store long duplicate draft and revised plan bodies unless the task
is so high-risk that preserving both bodies is itself useful evidence. The
default plan artifact should keep the final executable plan plus compact
review evidence fields and short notes.

Required review evidence fields for review-required plans:

```text
Risk triggers matched: <list of cited triggers from § Non-Trivial Triggers>
Importance/Risk: trivial / non-trivial
Domain self-mark: <conditional domain checklist; non-trivial only>
Review required: yes
Plan reviewed: yes
Personas applied: <non-empty persona list>
Revised after review: yes
```

Review-required plans must also include a short section:

```text
## Separate Persona Review Passes

### <Persona name>

- Severity: must-change / should-change / note
- Plan risk found: <concrete risk>
- Required plan change: <specific plan edit>
- Verification or gate to add: <test, check, approval, or explicit none>
- Residual risk if accepted: <remaining risk>
```

For trivial plans, the only required persona pass is the Trigger Coverage
Verifier:

```text
Risk triggers matched: (none — trivial)
Importance/Risk: trivial
Review required: yes (Trigger Coverage Verifier only)
Plan reviewed: yes
Personas applied: Trigger Coverage Verifier
Revised after review: yes / no changes required
```

If review is not required (plan-exempt), record:

```text
Review required: no
Review exemption reason: <short reason>
```

Use `Revised after review: no changes required` only when the separate
persona passes found no must-change or should-change items.

Do not mark `Plan reviewed: yes` until the separate persona passes are
actually performed and recorded.

If the agent discovers during implementation that:

- review was skipped or compressed,
- a plan was falsely marked reviewed before persona passes ran,
- a trivial classification was wrong (a non-trivial trigger matches but was
  not cited), or
- the scope has expanded mid-execution to match a non-trivial trigger that
  was not present at the original classification,

implementation must stop. The agent reclassifies under the current plan
content, updates the plan's preflight fields and Domain Coverage to match
the reclassification, re-runs persona review per the new classification
(full review for non-trivial; Trigger Coverage Verifier only for trivial),
records the correction in the active plan when governance-relevant, and
resumes execution only from the revised plan.

## Persona Review Contract

Each persona review must produce plan edits, not general commentary.

Expected output per persona:

```text
Persona:
Severity: must-change / should-change / note
Plan risk found:
Required plan change:
Verification or gate to add:
Residual risk if accepted:
```

## Severity

- `must-change`: the plan should not execute until fixed.
- `should-change`: fix if cheap; execution can proceed only if the risk is
  accepted.
- `note`: useful observation, not a blocker.

## Selection Rule

1. Run the classification gate.
2. If planning is not required, do not run persona review.
3. For trivial plan-required work, run only the Trigger Coverage Verifier
   persona.
4. For non-trivial plan-required work:
   a. Run the Trigger Coverage Verifier first.
   b. Then run the universal bench personas sequentially.
   c. Then run substance-triggered personas sequentially. A substance-
      triggered persona is invoked only if its substance is marked as
      applicable in the plan's Domain Coverage section, or if the Trigger
      Coverage Verifier flagged a missing mark that should have been set.
5. If a plan-exempt class escalates, treat it as non-trivial plan-required
   and apply step 4.
6. If the class is conditional, use the corresponding deck per the
   triggers in `task-classification.md`.
7. If a task has multiple classes, union the substance-triggered personas
   and remove duplicate roles. Run sequentially.
8. Prefer fewer strong personas over many shallow personas. Never add a
   persona that cannot produce a concrete plan change.

## Consolidation

After persona reviews, consolidate findings into these buckets:

- Must change before execution.
- Nice to improve if cheap.
- Accept as known residual risk.

Then revise the plan once. Avoid another review loop unless a must-change item
creates a new irreversible or high-stakes risk.

The consolidation output should be short:

- must-change findings
- changes applied
- residual risks

Avoid copying the full pre-review plan into the final plan body.

## Finding Quality Bar

A useful finding includes at least one of:

- a missed step
- a wrong sequence
- a missing verification
- a missing user gate
- a missing source or authority check
- a rollback or recovery gap
- a scope leak
- a safe simplification

Do not keep weak findings that only restate generic caution.
