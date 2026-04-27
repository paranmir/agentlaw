# Lookup Rules

This file is starter seed memory. The authoritative session-level workflow lives in `docs/harness/MEMORY_AND_CONTINUITY_RULES.md` ("Canonical Restore Route" and "Canonical Save Route"); this file aligns to that workflow at the lookup-policy level. Specialize values for the local project after install.

## Read Order
1. `memory/working-set.md`
2. `memory/LOOKUP_RULES.md` when a retrieval choice needs explicit policy
3. active rules under `memory/rules/` whose scope matches the session and whose `applies_when` is `always` or matches a current trigger; the restore packet surfaces these in its `active_rules` field so this step is effectively free when restoring via `harness_session_restore`
4. relevant active known facts under `memory/known-facts/`
5. relevant preferences in `memory/preferences.md`
6. recent logs under `memory/logs/*` only when current state is insufficient
7. source files referenced by selected memory

## Trigger Map
- session_restore: walk the Canonical Restore Route in `docs/harness/MEMORY_AND_CONTINUITY_RULES.md`. Use `harness_session_restore` MCP tool when available; otherwise walk the route manually.
- session_save: walk the Canonical Save Route in `docs/harness/MEMORY_AND_CONTINUITY_RULES.md`. Use `harness_session_save` MCP tool when available; otherwise walk the route manually.
- law_question: read law files first; use memory only as pointers
- implementation_question: working set, active plan, known facts, then source files
- user_preference: preferences, then source check if preference conflicts
- recall_query: prefer `memory_search` MCP tool over manual file scans
- source_confirmation: use `memory_read_source` for exact canonical memory text when the source object is one of `working_set`, `lookup_rules`, `preferences`, `known_fact`, or `log_entry`; direct file reads are the degraded fallback only when MCP is unavailable
- rule_applicability: `applies_when` values are opaque strings matched exactly. Law enumerates only `always`; any other trigger vocabulary is defined inside the rule body by the project that authored the rule. A rule fires when the session's current trigger set contains the rule's `applies_when` string verbatim — there is no wildcard, substring, or taxonomy match.

## Budgets
- working_set_max_tokens: 1200
- memory_packet_max_tokens: 2500
- log_lookback_days_default: 14
- top_k_known_facts: 5
- top_k_logs: 5

## Exclusions
- exclude statuses: stale, superseded, disputed, quarantined
- exclude wrong-scope memory unless explicitly requested
- exclude logs as current truth unless confirmed by active sources
- exclude runtime/index/cache output as authority

## Escalation
- read law before memory when authority is unclear
- read canonical memory source with `memory_read_source` when derived memory is tentative or exact memory text is needed
- when `memory/working-set.md` is missing, stale, or unavailable, mark restore as degraded and rebuild from logs and source files
- inspect derived runtime/index state via `memory_runtime_check` when source drift is suspected; repair it via `memory_runtime_repair` only when recovery is required
