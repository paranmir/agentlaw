# Task Classification

## Purpose

Classify chatbot requests before deciding whether to use the full planning and
persona-review workflow.

The goal is to avoid heavy planning for trivial work while still forcing
structured planning for multi-step, state-changing, high-risk, current-source,
or externally consequential work.

## Quick Gate

Skip the full planning workflow only when all of these are true:

- The task is answer-only.
- The expected output is short.
- The stakes are low.
- No file, system, browser, repository, package, deployment, account, payment,
  message, or external state will change.
- No current or source-verified information is required.
- No legal, medical, financial, safety, security, employment, housing, or
  similarly consequential decision is involved.
- The answer can be produced from current conversation context.
- The user did not ask for a structured plan.

Use the planning workflow when any of these are true:

- The task requires multiple dependent steps.
- Tool use will mutate files, system state, browser state, remote repositories,
  deployments, packages, accounts, payments, messages, or other external state.
- The action is irreversible or hard to reverse.
- The answer depends on current facts, prices, laws, schedules, versions, APIs,
  or other time-sensitive information.
- The domain is high-stakes.
- The result will be used for a significant decision involving money, time,
  reputation, production systems, or user data.
- The task spans multiple artifacts, files, stakeholders, or phases.
- The task changes governance, memory, policy, release state, or process rules.
- The task requires validation through tests, citations, calculations, previews,
  dry-runs, or external confirmation.
- The task has ambiguous requirements where wrong assumptions would be costly.

## Plan-Exempt Classes

These classes normally skip the full planning workflow when small and low-risk.
Escalate if they gain risk, state changes, source freshness requirements,
formality, or durable process impact.

| Class | Skip when | Escalate when |
| --- | --- | --- |
| `simple_information_answer` | stable, short, low-risk fact | high-stakes, current, sourced, or formal answer |
| `simple_explanation` | short concept or code-line explanation | curriculum, long tutorial, work-critical guidance |
| `simple_translation` | short ordinary text | official, contractual, regulated, technical, or brand-sensitive text |
| `simple_rewrite` | short local tone or clarity edit | public, persuasive, sensitive, legal, or long-form rewrite |
| `small_summarization` | short input without traceability needs | long, technical, legal, decision-critical, or evidence-preserving input |
| `light_brainstorming` | exploratory ideas with low failure cost | brand strategy, campaign, launch, or product naming |
| `casual_conversation` | no durable output or action | user asks to remember, formalize, publish, or operationalize |
| `format_conversion` | small mechanical transformation | files, schemas, migrations, documents, or downstream systems are affected |
| `minor_clarification` | current conversation context is enough | older session state, memory, files, or external sources are needed |

## Plan-Required Classes

These classes normally require a plan before execution.

| Class | Planning reason |
| --- | --- |
| `research_web_or_source_based` | source selection, freshness, conflict resolution, and citations matter |
| `decision_support_high_impact` | criteria, tradeoffs, uncertainty, and user constraints matter |
| `complex_planning_strategy` | the task is planning itself and needs dependencies, risks, and milestones |
| `data_analysis` | data quality, method choice, calculation correctness, and presentation matter |
| `coding_implementation` | file edits, tests, architecture, and regression risk matter |
| `debugging_troubleshooting` | symptom, reproduction, root cause, fix, and verification must stay separate |
| `code_review` | findings must be prioritized, evidence-backed, and risk-ranked |
| `automation_tool_use_with_side_effects` | commands may mutate files, systems, accounts, or remote state |
| `file_document_work` | artifact edits often need render, preview, formula, or compatibility checks |
| `substantial_writing_editing` | audience, purpose, structure, tone, and approval criteria matter |
| `substantial_translation_localization` | terminology, locale, tone, and fidelity matter |
| `substantial_summarization_extraction` | large inputs need extraction rules and traceability |
| `substantial_creative_generation` | direction, constraints, variants, and selection criteria matter |
| `substantial_image_visual_work` | visual direction, exact text, asset constraints, and QA matter |
| `product_recommendation` | spending time or money requires current, user-specific comparison |
| `legal_financial_medical_sensitive` | safe boundaries, current authority, and professional escalation matter |
| `personal_advice_coaching_sensitive` | safety, autonomy, emotional support, and escalation may matter |
| `conversation_memory_continuity` | persistence, recall, authority, and privacy boundaries matter |
| `release_deploy_external_action` | external release states can be irreversible |
| `meta_process_improvement` | workflow, memory, governance, or tool changes affect future behavior |

## Conditional Classes

These classes are plan-exempt when small and low-risk, but plan-required when
they become large, consequential, state-changing, or formal.

| Class | Plan when |
| --- | --- |
| `explanation_teaching` | curriculum, long tutorial, exam prep, or work-critical guidance |
| `writing_editing` | formal, public, sensitive, persuasive, or long-form writing |
| `translation_localization` | official, technical, regulated, or brand-sensitive text |
| `summarization_extraction` | long input, decision support, evidence retention, or action items |
| `creative_generation` | campaign, brand, product, or long-form creative system |
| `automation_tool_use` | file, system, remote, account, or config state changes |
| `image_visual_work` | brand, commercial, UI, exact text, or artifact editing |
| `personal_advice_coaching` | safety, mental health, life decision, money, employment, or abuse risk |

## Classification Output

Use this internally for simple tasks. Surface it when it helps the user evaluate
the plan.

```text
Task class:
Plan required: yes/no
Reason:
Risk level: low/medium/high
Primary output contract:
State changes:
Freshness/source requirement:
User gate required:
```
