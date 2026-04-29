# Specialized Persona Decks

## Purpose

Select specialized personas after classifying the task. Use only the sections
that match the current task class.

## Escalated Plan-Exempt Classes

| Class | Personas |
| --- | --- |
| `simple_information_answer` | Fact Authority Auditor; Harm Context Reviewer; Concision Contract Reviewer |
| `simple_explanation` | Learner Level Reviewer; Example Fit Reviewer; Over-Teaching Reviewer |
| `simple_translation` | Meaning Fidelity Reviewer; Register Reviewer; Sensitive Language Reviewer |
| `simple_rewrite` | Intent Preserver; Tone Calibrator; Risk Language Reviewer |
| `small_summarization` | Key Point Selector; Detail Preserver; Evidence Need Reviewer |
| `light_brainstorming` | Novelty Reviewer; Constraint Reviewer; Selection Helper |
| `casual_conversation` | Memory Intent Reviewer; Boundary Reviewer; Actionability Reviewer |
| `format_conversion` | Schema Fit Reviewer; Data Loss Reviewer; Downstream Compatibility Reviewer |
| `minor_clarification` | Context Sufficiency Reviewer; Recall Accuracy Reviewer; Scope Guard |

## Plan-Required Classes

### `research_web_or_source_based`

Personas: Source Authority Reviewer; Freshness Reviewer; Conflict Resolution
Reviewer; Citation Quality Reviewer; Bias and Incentive Reviewer.

Plan changes to consider: add primary-source preference, source freshness date,
conflict handling, and citation requirements.

### `decision_support_high_impact`

Personas: User Constraint Elicitor; Option Set Completeness Reviewer; Tradeoff
Economist; Worst-Case Scenario Reviewer; Decision Framing Reviewer.

Plan changes to consider: add decision criteria, missing-info questions,
alternatives including doing nothing, and residual uncertainty.

### `complex_planning_strategy`

Personas: Goal Decomposer; Dependency Mapper; Resource and Capacity Planner;
Risk Buffer Designer; Stakeholder Reviewer; Retrospective Designer.

Plan changes to consider: add milestones, dependency order, checkpoints, stop
criteria, and escalation triggers.

### `data_analysis`

Personas: Data Provenance Reviewer; Data Quality Auditor; Methodology Reviewer;
Statistical Skeptic; Reproducibility Engineer; Visualization Critic.

Plan changes to consider: add data audit, assumption log, reproducible
calculation path, and limitations section.

### `coding_implementation`

Personas: Product Requirement Reviewer; Architecture Maintainer; API and
Compatibility Reviewer; Security Engineer; Test Strategy Reviewer; Operations
Reviewer; Minimal Diff Reviewer.

Plan changes to consider: add behavior acceptance criteria, file ownership
boundaries, security checks, test plan, and rollback path.

### `debugging_troubleshooting`

Personas: Symptom Reproducer; Hypothesis Triage Reviewer; Environment
Detective; Root Cause Reviewer; Fix Scope Reviewer; Regression Guard Reviewer.

Plan changes to consider: add reproduction step, hypothesis list, diagnostic
commands, fix validation, and recurrence guard.

### `code_review`

Personas: Product Intent Reviewer; Behavior Regression Reviewer; Security
Threat Modeler; Abuse Case Reviewer; Test Gap Prosecutor; Performance and Scale
Reviewer; Maintainability Reviewer; Security Report Reviewer; Release Manager.

Plan changes to consider: add file/line review strategy, severity rubric,
evidence requirement, test-gap checklist, and residual risk section.

### `automation_tool_use_with_side_effects`

Personas: Command Safety Reviewer; Permission Boundary Reviewer; Idempotency
Reviewer; Dry-Run Designer; State Snapshot Reviewer; Operator UX Reviewer.

Plan changes to consider: add exact command list, approval gate, dry-run or
preview, backup or state snapshot, and post-command verification.

### `file_document_work`

Personas: Artifact Contract Reviewer; Content Structure Reviewer; Formatting
and Layout Reviewer; Compatibility Reviewer; Data Integrity Reviewer; Render QA
Reviewer.

Plan changes to consider: add artifact spec, render/preview verification,
formula or metadata checks, and original-file preservation.

### `substantial_writing_editing`

Personas: Audience Strategist; Argument Architect; Voice and Tone Reviewer;
Risk Language Reviewer; Editor for Brevity; Approval Path Reviewer.

Plan changes to consider: add audience and purpose, outline, tone constraints,
sensitive-language review, and revision pass order.

### `substantial_translation_localization`

Personas: Locale Specialist; Terminology Manager; Meaning Fidelity Reviewer;
Cultural Risk Reviewer; Layout and Expansion Reviewer; Back-Translation
Reviewer.

Plan changes to consider: add locale, register, glossary, fidelity review, and
back-translation or human review gate.

### `substantial_summarization_extraction`

Personas: Objective Framer; Coverage Auditor; Critical Detail Preserver;
Evidence Linker; Bias and Compression Reviewer; Action Item Verifier.

Plan changes to consider: add extraction schema, preserve-list, source anchors,
ambiguity bucket, and action-item validation.

### `substantial_creative_generation`

Personas: Creative Director; Brand Strategist; Constraint Keeper; Direction
Diversity Reviewer; Feasibility Producer; Risk and Sensitivity Reviewer.

Plan changes to consider: add creative brief, direction axes, selection
criteria, risk filter, and iteration plan.

### `substantial_image_visual_work`

Personas: Visual Art Director; Brand and Usage Reviewer; Text Accuracy
Reviewer; Accessibility Reviewer; Asset Rights Reviewer; Technical Output
Reviewer; QA Preview Reviewer.

Plan changes to consider: add visual brief, asset specs, rights/safety check,
preview loop, and fallback to SVG, HTML, CSS, or diagram syntax when better.

### `product_recommendation`

Personas: Needs Elicitor; Market Freshness Researcher; Specification Skeptic;
Review Quality Auditor; Total Cost Reviewer; Fit and Tradeoff Reviewer; Safety
and Support Reviewer.

Plan changes to consider: add user constraints, current-source lookup,
comparison table, conditional recommendation, and avoid/edge-case section.

### `legal_financial_medical_sensitive`

Personas: Jurisdiction and Context Reviewer; Current Authority Reviewer; Scope
Limiter; Harm Scenario Reviewer; Red Flag Escalation Reviewer; Documentation
Reviewer; Professional Referral Reviewer.

Plan changes to consider: add high-stakes boundary, context questions, official
source lookup, red-flag handling, and professional consultation path.

### `personal_advice_coaching_sensitive`

Personas: Safety Triage Reviewer; Autonomy Preserver; Emotional Reality
Reviewer; Practical Next-Step Coach; Support Network Reviewer; Boundary and
Privacy Reviewer.

Plan changes to consider: add safety check, values clarification, low-risk next
step, escalation path, and tone constraint.

### `conversation_memory_continuity`

Personas: Authority Layer Reviewer; Recall Accuracy Reviewer; Persistence
Justification Reviewer; Privacy and Scope Reviewer; Working Set Discipline
Reviewer; Promotion Candidate Reviewer.

Plan changes to consider: add restore/search step, memory write or non-save
decision, scope/status choice, promotion proposal check, and post-save
verification.

### `release_deploy_external_action`

Personas: Release Manager; Irreversibility Gatekeeper; Build Artifact Auditor;
Preflight Verification Reviewer; Postflight Verification Reviewer; Rollback and
Hotfix Planner; Credential and Token Reviewer; Communications Reviewer.

Plan changes to consider: add version existence check, pre-upload gates,
explicit approval before irreversible steps, post-release smoke, and durable
release record.

### `meta_process_improvement`

Personas: Workflow Designer; Overhead Skeptic; Governance Fit Reviewer;
Adoption Reviewer; Measurement Reviewer; Conflict Reviewer; Promotion Path
Reviewer.

Plan changes to consider: add failure-mode statement, applicability scope,
trial/reference phase, promotion criteria, and lightweight exceptions.

## Conditional Classes

| Class | Lightweight personas | Escalate to |
| --- | --- | --- |
| `explanation_teaching` | Learner Level Reviewer; Pedagogy Sequencer; Transfer Reviewer | `complex_planning_strategy` when curriculum, exam prep, or work-critical guidance appears |
| `writing_editing` | Intent Preserver; Tone Calibrator; Clarity Editor | `substantial_writing_editing` |
| `translation_localization` | Naturalness Reviewer; Register Reviewer; Meaning Guard | `substantial_translation_localization` |
| `summarization_extraction` | Key Point Selector; Detail Preserver; Use-Case Reviewer | `substantial_summarization_extraction` |
| `creative_generation` | Novelty Reviewer; Constraint Reviewer; Selection Helper | `substantial_creative_generation` |
| `automation_tool_use` | Read-Only Safety Reviewer; Command Precision Reviewer; Stop Condition Reviewer | `automation_tool_use_with_side_effects` |
| `image_visual_work` | Prompt Clarity Reviewer; Medium Fit Reviewer; Visual QA Reviewer | `substantial_image_visual_work` |
| `personal_advice_coaching` | Empathy and Tone Reviewer; Practicality Reviewer; Boundary Reviewer | `personal_advice_coaching_sensitive` |

## Multi-Class Examples

| Task | Decks |
| --- | --- |
| Code review with security implications | Core bench, `code_review`, and `release_deploy_external_action` if release-gating applies |
| Product recommendation for a costly device | Core bench, `product_recommendation`, `decision_support_high_impact`, and `research_web_or_source_based` |
| Legal-adjacent document rewrite | Core bench, `substantial_writing_editing`, `legal_financial_medical_sensitive`, and localization deck if multilingual |
| Release with package metadata changes | Core bench, `release_deploy_external_action`, `coding_implementation`, `automation_tool_use_with_side_effects`, and `conversation_memory_continuity` |
