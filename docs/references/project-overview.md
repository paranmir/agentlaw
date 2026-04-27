# Project Overview

_This file is the harness-kit project-overview template. Fill in the required sections after `harness-kit init`. Keep optional sections only if they add real value; delete the ones that do not apply._

_Role boundary: this file is reference-layer orientation. It must not duplicate `docs/harness/HARNESS_SCOPE.md` (law-layer scope), `AGENTS.md` (execution-entry routing), or `README.md` (installation and usage)._

## What this is

_(fill in after harness-kit init — 1 to 3 sentences summarizing what this project is. Keep it factual. Do not include installation instructions; those belong in README.)_

## Why it exists

_(fill in after harness-kit init — the motivation: the problem being solved or the reason the project exists. Keep it short.)_

## Audience

_(fill in after harness-kit init — who uses this project, who develops it, and who operates it. One line per group is usually enough.)_

## Project-specific additions to the standard Harness layout

_(optional — fill in only if this project adds directories or files beyond the harness default. Do not restate the default layout defined in `docs/harness/REPOSITORY_ARTIFACT_RULES.md`. Delete this section if there are no project-specific additions.)_

## Project-specific domain glossary

_(optional — define terms unique to this project's domain. Do not include harness common vocabulary such as `law`, `memory`, `runtime repair`, or `constitution`; those are owned by the constitution and law layer. Delete this section if no project-specific terms exist.)_

## Code architecture map

_Obligation: see `docs/harness/CODE_AUTHORSHIP_AND_STEWARDSHIP_RULES.md` "Code Architecture Map". Populate this section once the project has code worth mapping; leave the placeholder in place until then — the harness verifier silently skips the freshness check while no `Map scope:` block is declared._

_Slot-selection rationale: (fill in — one short paragraph naming which slots this project uses and why. Required slots: at least one structural (Module dependency or Class diagram) and at least one flow (Entry-point call graph, State diagram, or Data flow). Library-only projects may omit the flow slot with a one-line rationale recorded here.)_

_**Map scope:**_
- _(fill in glob patterns that cover this project's source tree, e.g. `src/**/*.py`. Delete this bullet once real patterns are added.)_

_For each slot you use, create a subsection below with a heading naming the slot (e.g. "### Module dependency", "### Entry-point call graph — <command name>") and a Mermaid block inside. Every diagram must carry a slot-name heading, respect the per-diagram cap of 15 nodes and 25 edges, and split via boundary nodes (dashed-border placeholders) plus a node index table when the cap would be exceeded. Mermaid is the only permitted format for this section._

## Data / state model

_(optional — an ASCII sketch of durable state owned by this project. Use plain ASCII only; Mermaid or other rendered diagrams are out of scope for this template. Example shape:_

```text
canonical file -> derived table -> serving surface
```

_Delete this section if the project has no durable state worth explaining at overview level.)_

## Typical flows

_(optional — ASCII pipelines for one or two primary use cases. Keep each flow small enough to read at a glance. Delete this section if there is no meaningful flow beyond what the code and AGENTS.md already describe.)_
