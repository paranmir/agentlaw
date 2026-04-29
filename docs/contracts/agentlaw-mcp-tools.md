---
status: draft
updated_at: 2026-04-20T22:55:00+09:00
scope: repository
---

# Harness MCP Tool Surface (DRAFT v1)

## Authority
This document is a contract document. It is the source of truth
for the MCP tool surface exposed by the agentlaw-memory server, shared with target projects via `publish-repo/`,
and its consistency with `src/agentlaw/server/tools/` is mechanically enforced
by `agentlaw verify` (`_test_publish_seed_contract_finite_set` and tool-coverage check).

Governing law: `docs/law/MEMORY_AND_CONTINUITY_RULES.md`. Amendments land
through a plan that updates this file and any dependent law
clause in the same change.

## Purpose
Implementation reference for the MCP tools exposed by the `agentlaw` pip package's bundled MCP server. This document defines the *signature* of each tool: name, parameters, return shape, error semantics. The *when and why* for each tool is governed by [`docs/law/MEMORY_AND_CONTINUITY_RULES.md`](../harness/MEMORY_AND_CONTINUITY_RULES.md) ("Memory Tool Surface (MCP)" section).

This is a draft and lives in `docs/contracts/`. Once stable it moves to `docs/law/` as a governed protocol document.

## Tool Operations Authority

- All tools operate on `.harness/index/meta.db` (SQLite + FTS5 + sqlite-vec) and the canonical Markdown layer under `memory/*`.
- Write tools must write the canonical Markdown layer first, then update derived DB/index rows in a SQLite transaction. If the canonical write succeeds but the derived update fails, the canonical Markdown remains the source of truth and the tool must return visible derived-index drift with a repair path.
- Tools must never mutate `docs/law/*`, `AGENTLAW_CONSTITUTION.md`, root control documents, or `plans/*`. Memory cannot promote itself into law; only `memory_propose_promotion` may suggest promotion.
- Tools must respect the conflict resolution rules in `MEMORY_AND_CONTINUITY_RULES.md` (`authority > scope > recency`, status transitions, exclusion rules).

## Common Conventions

- All timestamps are ISO 8601 UTC unless stated otherwise.
- All ids are stable slugs persisted in markdown front matter (see schema reference).
- All tools return a `runtime_status` field reporting whether the index, vector store, and embedding model are available.
- When a tool cannot complete due to missing runtime, it returns a structured `degraded` result rather than raising; the caller decides how to fall back.
- Closed-set parameters (`type`, `scope`, `status`, `to_status`, `kind`, `mode`, `order_by`, `target_kind`) advertise their permitted values via JSON Schema `enum` constraints. MCP hosts may reject invalid values at the schema layer before the tool is invoked. The server still validates every closed-set parameter at runtime — schema enforcement is advisory; the server remains authoritative.

### Error Codes
Expected Harness domain failures are returned in-band in the tool result as an `error` object plus `runtime_status` when available:

```json
{
  "error": { "code": "memory.not_found", "id": "fact/missing-example" },
  "runtime_status": {}
}
```

Unexpected transport, protocol, or server failures still follow JSON-RPC 2.0 (MCP transport):

- `-32700` Parse error
- `-32600` Invalid Request
- `-32601` Method not found
- `-32602` Invalid params
- `-32603` Internal error
- `-32000` to `-32099` Application errors (server-defined).

Domain codes used in this surface include:

| Code | When it is raised |
| --- | --- |
| `memory.not_found` | An id was provided but does not exist in `memory_items` or `memory_logs`. |
| `memory.governance_violation` | A write tool was asked to modify a path reserved for governance (e.g., `docs/law/`, root control documents, `plans/`). |
| `memory.invalid_transition` | A lifecycle operation is rejected because the target state is not reachable from the current state (e.g., `memory_supersede` where the replacement id is not `active`). |
| `memory.read_only_violation` | An append-only resource (log entry) was asked to be overwritten. |
| `memory.runtime_unavailable` | A required runtime dependency is not available (missing/corrupt DB, embedding model missing in a path that mandated it). |
| `memory.derived_index_drift` | A canonical Markdown write succeeded, but the derived DB/index update failed. The response includes the markdown path and a `memory_runtime_repair` scope. |
| `memory.runtime_repair_conflict` | A runtime repair request was rejected because another runtime repair job is already queued or running. |
| `memory.job_stale` | A queued or running runtime repair job exceeded the stale threshold and is treated as abandoned/failed. |
| `memory.invalid_params` | A parameter fails a general validation check not covered by a more specific code (e.g., unknown `type`, `scope`, or `status` value; `old_id == new_id`). |
| `memory.invalid_datetime` | A timestamp or date-bearing value violates the Timestamp Integrity law (see `docs/law/MEMORY_AND_CONTINUITY_RULES.md`), e.g., a caller-supplied log id whose date segment does not match today's UTC date. |
| `memory.not_implemented` | Reserved for future deferrals — a documented tool parameter path not yet implemented in the current runtime. No tool parameter currently returns this code. |

### Pagination Details
List and search tools return a `next_cursor` (opaque string). Pass it as `cursor` on the next call to fetch the next page. Cursor-based pagination is stable under concurrent writes; offset-based pagination is not supported.

```
memory_search(query="...", limit=10)
→ { hits: [...], next_cursor: "abc123" }

memory_search(query="...", limit=10, cursor="abc123")
→ { hits: [...], next_cursor: "def456" }
```

When `next_cursor` is null, the result set is exhausted.

### Concurrency And Multi-Agent
Each MCP server process opens `.harness/index/meta.db` in WAL mode (`PRAGMA journal_mode=WAL`) with `PRAGMA busy_timeout=5000`. Multiple agent processes may share the same database file safely:

- Concurrent reads are non-blocking.
- Writes are serialized by SQLite; brief lock waits are normal.
- No application-level authentication in v1; the server trusts its caller (single-tenant per agent process).

If per-agent attribution is needed later, a `created_by` field may be added through a schema migration without breaking existing tools.

---

## Read Tools

### `memory_search`
Hybrid FTS + vector search across memory items and logs. Use for prior judgment, decisions, rationale, or cross-session continuity questions; for current source/law facts use Grep / Read instead. Routing criterion: `docs/law/MEMORY_AND_CONTINUITY_RULES.md` §Read Routing Criterion.

Logs are historical records and are not default vector-search targets in v1. Results from the FTS5 path and the sqlite-vec path are blended via **Reciprocal Rank Fusion (RRF, k=60)** at query time. RRF uses each hit's rank in the two lists; absolute score scales are ignored, so no normalization is required and no blended score is stored. RRF score per item: `Σ 1/(k + rank_in_list)` across the FTS and vector lists.

**Parameters**
- `query` (string, required) — natural-language query, KO/EN mixed allowed.
- `types` (array of strings, optional) — filter by `fact` / `preference` / `rule` / `log`.
- `scopes` (array of strings, optional) — filter by `repository` / `session` / `global`.
- `tags` (array of strings, optional) — restrict to entries carrying any of these tags.
- `statuses` (array of strings, optional) — defaults to `["active"]`. Override to include other statuses. Allowed values: `active` / `tentative` / `stale` / `superseded` / `disputed` / `suppressed` / `quarantined`.
- `time_window_days` (integer, optional) — restrict to entries updated within N days.
- `limit` (integer, optional, default 10) — maximum hits returned.
- `mode` (string, optional, one of `hybrid` / `fts` / `vector`, default `hybrid`).
- `cursor` (string, optional) — opaque pagination cursor from a previous response.

**Returns**
- `hits` — array of `{ id, type, scope, status, anchor, snippet, path, rrf_score, ranks: { fts, vector } }`. `ranks` records each item's 0-based rank in the source lists (null when absent from a list).
- `next_cursor` (string or null).
- `query_token_estimate`, `result_token_estimate`.
- `runtime_status`.

**Errors**
- `memory.invalid_params` for an empty query, unknown mode, invalid filter value, invalid limit, or malformed cursor.
- `memory.runtime_unavailable` when `mode="vector"` requires the embedding/vector runtime and it cannot load, encode, or query.
- In `mode="hybrid"`, vector runtime failures degrade to FTS-only results with a `degraded` note.

---

### `memory_get`
Fetch a single item or log entry by id. Direct id-keyed lookup; no routing decision. Ids come from `memory_save_item` / `memory_append_log` responses, plan or working-set references, `memory_search` / `memory_recent_logs` / `memory_list` results, or `agentlaw_session_save.log_entry_id` — `memory_search` is not a prerequisite when an id is already known.

**Parameters**
- `id` (string, required).
- `include_chunks` (boolean, optional, default false) — include chunk-level body breakdown.

**Returns**
- `entry` — full record from `memory_items` or `memory_logs` (kind inferred from id).
- `chunks` — present only when `include_chunks=true`.
- `runtime_status`.

**Errors**
- `not_found` if id does not exist.

---

### `memory_read_source`
Read a closed-set canonical memory source object from `memory/*`.

This tool returns canonical Markdown source text for specific memory source objects. It does not accept filesystem paths and is not a general Markdown, repository file, law, plan, reference, or source-code reader.

**Parameters**
- `kind` (string, required) — one of `working_set`, `lookup_rules`, `preferences`, `known_fact`, or `log_entry`.
- `id` (string, required only for `known_fact` and `log_entry`) — `fact/<slug>` for known facts, or `log/YYYY-MM-DD/<slug>` for log entries.
- `max_chars` (integer, optional, default 12000, maximum 50000) — maximum source characters returned. Larger sources return `truncated: true`.

Singleton kinds (`working_set`, `lookup_rules`, `preferences`) reject `id`.

**Returns**
- `kind`, `id`, `path`.
- `canonical` — always true when a source object is returned.
- `source_authority` — `memory`.
- `authority_warning` — reminder that memory is lower authority than law, root controls, plans, references, and current repository state.
- `content` — canonical Markdown source text or bounded prefix.
- `front_matter` — parsed YAML front matter for singleton/known-fact files, or parsed entry metadata for log entries.
- `content_hash` — SHA-256 hash of the full canonical source text.
- `truncated` — whether `content` was bounded by `max_chars`.
- `token_estimate`.
- `runtime_status`.

**Errors**
- `memory.invalid_params` for unknown kind, malformed id, missing required id, id on singleton kinds, or invalid `max_chars`.
- `memory.not_found` when the requested closed-set memory source object does not exist.
- `memory.source_read_violation` when source resolution would leave the repository's canonical `memory/` namespace.

---

### `memory_list`
Filtered list of items by metadata. Use when the question is "what items of kind X exist" rather than free-text search. Routing criterion: `docs/law/MEMORY_AND_CONTINUITY_RULES.md` §Read Routing Criterion.

**Parameters**
- `types` (array, optional) — same semantics as `memory_search.types`. Allowed values: `fact` / `preference` / `rule`.
- `scopes` (array, optional) — same semantics as `memory_search.scopes`. Allowed values: `repository` / `session` / `global`.
- `statuses` (array, optional) — same semantics as `memory_search.statuses`. Allowed values: `active` / `tentative` / `stale` / `superseded` / `disputed` / `suppressed` / `quarantined`.
- `tags` (array of strings, optional) — same semantics as `memory_search.tags`.
- `applies_when` (array of strings, optional) — match `rule` items whose trigger tags include any of these values.
- `order_by` (string, optional, one of `updated_at_desc` / `created_at_desc`, default `updated_at_desc`).
- `limit` (integer, optional, default 50).
- `cursor` (string, optional) — opaque pagination cursor from a previous response.

**Returns**
- `items` — array of `{ id, type, scope, status, title, path, updated_at, tags }` (no body to keep packets small; use `memory_get` for body).
- `next_cursor` (string or null).
- `total` (integer) — total matching count even when limited.
- `runtime_status`.

---

### `memory_recent_logs`
Tail of the append-only log, newest first. Use to recover recent session context, decisions made earlier, or to verify whether a finding has already been logged before re-investigating. Routing criterion: `docs/law/MEMORY_AND_CONTINUITY_RULES.md` §Read Routing Criterion. Default time window 14 days.

**Parameters**
- `days` (integer, optional, default 14).
- `kinds` (array of strings, optional) — filter by `decision` / `correction` / `session_save` / `verification`.
- `tags` (array of strings, optional).
- `limit` (integer, optional, default 50).
- `cursor` (string, optional) — opaque pagination cursor from a previous response.

**Returns**
- `logs` — array of `{ id, kind, scope, title, path, occurred_at, tags }`.
- `next_cursor` (string or null).
- `runtime_status`.

---

## Write Tools

### `memory_save_item`
Create or update a durable memory item — rules (govern future behavior), facts (persist as current state), or preferences (user / maintainer style). Items are vector-indexed and shape `memory_search` results; poorly scoped items pollute semantic recall until marked stale. Selection criterion (stricter than logs): `docs/law/MEMORY_AND_CONTINUITY_RULES.md` §Item Write Criterion. For transient findings use `memory_append_log`.

Types `fact`, `rule`, and `preference` are all fully supported. `fact` and `rule` use one canonical file per item under `memory/known-facts/` or `memory/rules/` with YAML front matter + body. `preference` uses single-file section-aware editing of `memory/preferences.md`: each preference is one `## <slug>` section per the Machine-Writable Grammar in `docs/law/MEMORY_AND_CONTINUITY_RULES.md` §"Preferences File"; the writer rewrites only the target section's bytes, refreshes the file-level `updated_at` timestamp, and leaves all sibling sections byte-identical.

Calls with `type="working_set_entry"` return `memory.invalid_params` — the value is not a declared type at the tool layer. The working-set file is rewritten in full by `agentlaw_session_save` rather than addressed at item-level.

For all types: writes canonical Markdown first, then updates derived item/chunk/tag/vector rows.

**Parameters**
- `type` (string, required) — `fact` / `preference` / `rule`.
- `id` (string, optional) — required on update; on create the server generates a slug if omitted.
- `scope` (string, required) — `repository` / `session` / `global`.
- `status` (string, optional, default `active`) — one of `active` / `tentative` / `stale` / `superseded` / `disputed` / `suppressed` / `quarantined`.
- `title` (string, optional).
- `body` (string, required) — markdown body. For preferences, the body is the prose under the field-list block; the writer assembles the field list from `id` / `status` / `scope` / fixed defaults (`source: conversation`, `last_checked: <today>`, `applies_to: final_response`, `loses_to: [law, current_explicit_instruction, safety, verification]`).
- `tags` (array of strings, optional).
- `path` (string, optional) — target markdown path; the server picks a default by type when omitted (`memory/preferences.md` for preferences; per-slug paths for facts and rules).
- `supersedes` (string, optional) — id this entry supersedes (sets supersession links on both records).

**Returns**
- `entry` — the persisted record metadata (post-merge for updates). The
  body itself is **not** echoed; instead `entry.body_chars` carries the
  character count of the persisted body so callers can round-trip-check
  what was sent (`assert response["entry"]["body_chars"] == len(sent_body)`).
  Fetch the full body via `memory_get(id)` or read the canonical markdown
  at `markdown_path`.
- `created` (boolean).
- `markdown_path` — absolute path to the canonical markdown that was written.
- `chunks_written` (integer).
- `runtime_status`.
- On derived DB/index failure after the Markdown write: `error.code = memory.derived_index_drift`, `degraded`, and `drift = { canonical_written, markdown_path, repair_tool, repair_scope }`.

**Errors**
- `governance_violation` if `path` targets `docs/law/`, `AGENTLAW_CONSTITUTION.md`, root control docs, or `plans/*`.
- `derived_index_drift` if the canonical Markdown write succeeded but derived DB/index update failed.

---

### `memory_append_log`
Append-only log entry for session-spanning findings, decisions, and corrections. For durable rules or current-state facts, use `memory_save_item`. Selection criterion: `docs/law/MEMORY_AND_CONTINUITY_RULES.md` §Log Write Criterion. Logs are FTS-only and do not affect `memory_search` semantic ranking.

Writes canonical Markdown first to today's log file, then updates derived log/chunk/tag rows. V1 does not create log vector rows.

**Parameters**
- `kind` (string, required) — `decision` / `correction` / `session_save` / `verification`.
- `scope` (string, required) — `repository` / `session` / `global`.
- `title` (string, required).
- `body` (string, required).
- `tags` (array of strings, optional).
- `session_id` (string, optional).
- `id` (string, optional) — server generates if omitted.

**Returns**
- `entry` — the appended record metadata. The body itself is **not** echoed;
  `entry.body_chars` carries the character count of the persisted body for
  caller-side round-trip sanity. Fetch full body via `memory_get(id)` or
  read the canonical markdown at `markdown_path`.
- `markdown_path` — absolute path to today's log file.
- `chunks_written` (integer).
- `runtime_status`.
- On derived DB/index failure after the Markdown append: `error.code = memory.derived_index_drift`, `degraded`, and `drift = { canonical_written, markdown_path, repair_tool, repair_scope }`.

**Errors**
- `read_only_violation` if the caller attempts to update an existing log id.
- `derived_index_drift` if the canonical Markdown append succeeded but derived DB/index update failed.

---

## Lifecycle Tools

### `memory_set_status`
Transition an item's status following the conflict rules in `MEMORY_AND_CONTINUITY_RULES.md`.

**Parameters**
- `id` (string, required).
- `to_status` (string, required) — one of the seven allowed statuses: `active` / `tentative` / `stale` / `superseded` / `disputed` / `suppressed` / `quarantined`.
- `reason` (string, required) — recorded as a log entry of kind `correction`.

**Returns**
- `entry` — updated record.
- `log_entry_id` — the correction log entry that was appended.
- `runtime_status`.

**Errors**
- `invalid_transition` if the status change is not allowed by the conflict rules.

---

### `memory_supersede`
Mark an item as `superseded` and link it to a replacement in one operation.

**Parameters**
- `old_id` (string, required).
- `new_id` (string, required) — must already exist and be `active`.
- `reason` (string, required).

**Returns**
- `old_entry`, `new_entry` — both with supersession links set.
- `log_entry_id`.
- `runtime_status`.

---

## Governance Tool

### `memory_propose_promotion`
Propose escalating a memory entry into law, plan, reference, or shared artifact. Append-only proposal — writes a `promotion-proposal` log entry; **does not directly modify the target.** Selection criterion (three-property gate: durability + future operational relevance + authority gap): `docs/law/MEMORY_AND_CONTINUITY_RULES.md` §Promotion Proposal Protocol.

**Parameters**
- `source_ids` (array of strings, required) — memory ids to promote.
- `target_kind` (string, required) — `law` / `plan` / `reference` / `shared_artifact`.
- `target_path_suggestion` (string, optional).
- `rationale` (string, required).

**Returns**
- `proposal_id` — id of a `memory_logs` entry with kind `decision` and a tag `promotion-proposal`.
- `promotion_protocol` — the agent-applied promotion rule: required criteria, negative cases, source-id rule, and `runtime_selects_candidates=false`.
- `runtime_status`.

The proposal is a record only; it does not modify the target document set. Reviewed promotion goes through the normal change path (plan or direct law edit).
Runtime never selects semantic promotion candidates. The agent must call this tool only after judging that the source information has durability, future operational relevance, and an authority gap.

---

## Session Tools

### `agentlaw_session_restore`
Implements the Canonical Restore Route in `MEMORY_AND_CONTINUITY_RULES.md`.

**Parameters**
- `log_lookback_days` (integer, optional) — overrides the default from `LOOKUP_RULES.md`.
- `include_drift_check` (boolean, optional, default true).
- `recent_logs_limit` (integer, optional, default `15`) — cap `recent_logs` at the most-recent N entries within the lookback window. Pass `0` for unlimited (returns every entry inside the lookback window, matching the historical pre-cap behavior). The cap keeps a typical restore packet compact; use `0` only when a specific task genuinely needs the full history. The same flag exists on the CLI fallback as `agentlaw session-restore --recent-logs-limit N`.

**Returns**
- The full `agentlaw_session_restore` packet defined in `MEMORY_AND_CONTINUITY_RULES.md` ("Session Restore Packet Format"). Always includes `runtime_status`, `authority_warnings`, source pointers, excluded memory, and `token_estimate`.
- `active_plan_bodies` (array of objects) — one entry per file under `docs/plans/active/*` matching the order of `active_plans`. Each entry carries `path`, `title`, `body` (full file content, truncated above 30_000 chars with a marker line), `body_chars` (full file size), `truncated` (boolean). Entries that fail to read carry `error` instead of `body`. The runtime fetches these so the agent that consumes the packet has the body-level substance §Canonical Restore Route Mandatory Tier step 4 requires; the on-disk files remain authoritative when verification matters.
- `active_rule_bodies` (array of objects) — one entry per `active_rules` entry, matching that list by `id`. Each entry carries `id`, `scope`, `applies_when`, `summary`, `path`, `body` (full rule body, truncated above 12_000 chars with a marker line), `body_chars`, `truncated`. Entries that fail to read carry `error` instead of `body`. Satisfies Mandatory Tier step 7 — every active rule's body must reach the agent's working context, not just its summary.
- `preferences_body` (object or null) — the full content of `memory/preferences.md` as `{path, body, body_chars}`, or `null` when the file is absent (fresh project). Satisfies Mandatory Tier step 8.
- `lookup_rules_body` (object or null) — the full content of `memory/LOOKUP_RULES.md` as `{path, body, body_chars}`, or `null` when the file is absent. Satisfies Mandatory Tier step 9.
- `known_facts_manifest` (array of objects) — one entry per active known-fact (`type='fact'`, `status='active'`). Each entry carries `id`, `title`, `scope`, `path`, `last_checked`. **Bodies are NOT included** — the manifest gives a complete catalog of remembered facts so the agent does not invent or contradict known state, while bodies are pulled on demand under the Conditional Tier or via the working-frame search hits. Satisfies Mandatory Tier step 10.
- `working_frame_memory_hits` (array of objects) — top-K hits (default K=10) from a `memory_search` call composed from `current_goal + next_actions + open_questions` and filtered to `types=["fact"]`, `statuses=["active"]`. Each entry carries `id`, `title`, `path`, `snippet`, `rrf_score`. Empty list when the working frame is empty or the search engine is unavailable. Satisfies Mandatory Tier step 11; the search is the indexed-memory layer's intended retrieval path, paired with the manifest as a safety net.
- `latest_session_save_body` (object or null) — the body of the most recent `kind: session_save` log entry, with `id`, `path`, `title`, `occurred_at`, `body`, `body_chars`, `truncated`. Truncation cap is 8_000 chars with a `memory_get(id=...)` pointer for the full entry. `null` when no `session_save` log entry exists (e.g., fresh project). Recovers the directly preceding session's wrap-up summary so the agent does not have to issue a separate `memory_get` call before composing the first response.
- `token_estimate.packet_tokens` reflects the **full serialized packet** — including `runtime_status`, `authority_recall`, `governance_drift`, every per-entry field, and the body-level payloads (`active_plan_bodies`, `active_rule_bodies`, `preferences_body`, `lookup_rules_body`, `known_facts_manifest`, `working_frame_memory_hits`, `latest_session_save_body`). Clients comparing this figure against a fixed context-window budget must account for body-level reads on every restore; the Mandatory Tier guarantees the packet carries substance, not just paths.
**Framework reminder fields (always present on every restore packet)**

These fields are runtime-generated reminders bundled by `runtime_reminders_for_restore()` in `authority.py`. They do not persist to disk and do not depend on `memory/working-set.md` content; they are regenerated fresh on every restore so each session starts with the same framework guidance.

- `authority_recall` (object) — compact runtime guidance pointing to governing sources and the promotion protocol. Sub-keys:
  - `governing_sources` (array) — list of `{path, reason}` entries for the law layer (constitution, MEMORY_AND_CONTINUITY_RULES.md, REPOSITORY_ARTIFACT_RULES.md, agentlaw-mcp-tools.md).
  - `task_guidance` (array) — list of `{task, read: [path]}` entries advising which law files to read for common task categories (memory subsystem change, promotion decision, artifact structure change).
  - `promotion_protocol` (object) — see `promotion_reminder` schema below; same shape, embedded for the "promotion decision" task path.
  - `memory_intent_rule` (object) — same shape as `memory_intent_reminder` below, embedded for completeness.
  - `persistence_rule` (string) — short reminder that routine authority checks should not be persisted as durable Markdown records.
  This field does not include runtime-selected `promotion_candidates`.

- `write_discipline_reminder` (object) — universal framework reminder for the Write Discipline rule. Sub-keys:
  - `checklist` (array of strings) — per-turn questions the agent should run before composing a final response (log criterion, item criterion, read routing, read-cluster check, working-set field discipline at session-save time).
  - `criteria_anchors` (object) — pointers from semantic key (`log_write`, `item_write`, `read_routing`, `log_body_format`, `working_set_discipline`, `promotion`) to the corresponding `docs/law/MEMORY_AND_CONTINUITY_RULES.md §Heading` reference.
  - `rule` (string) — one-line restatement: "Silence is a valid answer when the criteria are not met. Volume is not the target; selectivity is."
  - `runtime_boundary` (string) — note that runtime surfaces the reminder; the agent judges whether each criterion is met.

- `restore_procedure_reminder` (object) — surfaces the §Canonical Restore Route Mandatory Tier procedure that every agent must follow on session restore. Sub-keys:
  - `rule` (string) — restatement of the timing rule.
  - `steps` (array) — one entry per Mandatory Tier step, each `{step, action, purpose}`. Steps cover runtime integrity check, working_set read, full law layer body read, every active plan body read, latest session_save body read, recent_logs title scan, every active rule body fetch, preferences body read, lookup_rules body read, known-facts manifest scan, working-frame memory_search, drift inspection, packet assembly, and gap surfacing.
  - `rule_anchor` (string) — `docs/law/MEMORY_AND_CONTINUITY_RULES.md §Canonical Restore Route — Mandatory Tier`.
  - `runtime_boundary` (string) — runtime surfaces the procedure and pre-fetches every body field the procedure asks for (`active_plan_bodies`, `active_rule_bodies`, `preferences_body`, `lookup_rules_body`, `known_facts_manifest`, `working_frame_memory_hits`, `latest_session_save_body`); the agent still owns step execution and gap-surfacing judgment.

- `consult_before_answer` (object) — read-side timing rule packet for the §Consult-Before-Answer Rule; pins the rule that memory-routed questions must consult memory **before** composing the answer, not as a follow-up. Sub-keys:
  - `rule` (string) — restatement of the rule.
  - `applies_when` (string) — trigger signal vocabulary (Q1/Q2 of §Read Routing Criterion: 'we', 'previously', 'earlier', '왜', '근거', '어디까지', 'outstanding', plus multilingual analogues). Q3 (current source / law fact) does NOT trigger.
  - `anchor` (string) — `docs/law/MEMORY_AND_CONTINUITY_RULES.md §Consult-Before-Answer Rule`.
  - `runtime_boundary` (string) — runtime surfaces the rule; the agent judges whether the question's shape triggers it and which read tool to call.

- `plan_discipline_reminder` (object) — planning workflow reminder for non-trivial work. Sub-keys:
  - `rule` (string) — compact plan-first obligation.
  - `non_trivial_triggers` (array of strings) — common cases that require a plan, such as law/root-control edits, public runtime surface changes, schema additions, and cross-concern commits.
  - `trivial_exemptions` (array of strings) — narrow cases where a repository-tracked plan is normally not required.
  - `anti_rationalization` (array of strings) — clauses that prevent auto-mode, broad user authorization, or elastic "trivial" interpretation from bypassing the plan gate.
  - `planning_workflow` (array of strings) — canonical sequence: `classify_request`, `draft_plan`, `review_with_persona_decks`, `synthesize_review_findings`, `produce_revised_plan`.
  - `classification_source` (string) — `docs/planning-protocol/task-classification.md`.
  - `review_method_source` (string) — `docs/planning-protocol/review-method.md`.
  - `persona_deck_sources` (array of strings) — `docs/planning-protocol/persona-decks-core.md` and `docs/planning-protocol/persona-decks-specialized.md`.
  - `mechanical_consequence` (string) — explains the plan-coverage verifier consequence for uncovered non-trivial changes.
  - `anchor` (string) — governing law anchors for the pre-edit and active-plan-field obligations.

- `suggested_queries` (array of strings) — topic-mined phrase list from recent log entries within the lookback window. Empty list when no recent logs exist (fresh session, fresh project, or `memory_logs` table missing). Otherwise, the top-K most-frequent tokens (default K=5) extracted from log titles + tags, weighted with linear recency decay so newer entries outrank older ones. The list is a starting-point reminder, not a curated retrieval — agents may use it to seed `memory_search` queries or ignore it if irrelevant. The phrase shape is short tokens (lowercased ASCII for English / digits, raw Hangul for Korean) suitable for direct use as `memory_search.query` substrings.

- `memory_intent_reminder` (object) — forward-looking reminder for the Memory Intent Rule, surfaced on every restore packet so the rule is refreshed at every session start. Sub-keys:
  - `triggers` (array of strings) — flat list of multilingual keyword stems (English, Korean, Japanese, Chinese, German, French, Spanish, Russian, Arabic) whose presence in user input may indicate memory intent. Word-level matching is heuristic; the agent uses surrounding context.
  - `triggers_by_language` (object) — same keywords keyed by ISO 639-1 language code, for callers that want per-language access.
  - `required_resolution` (array) — the four valid resolutions: `memory_write`, `promotion_proposal`, `associative_marker`, `explicit_non_save`. The `associative_marker` path covers the round-trip "user asks to remember; later asks 'what did I ask to remember?'" case where the substance already lives in another durable artifact and only a navigation breadcrumb is needed; see `docs/law/MEMORY_AND_CONTINUITY_RULES.md` §Memory Intent Rule for when each path applies.
  - `resolution_details` (object) — explanation for each resolution kind.
  - `prohibited` (string) — what is not allowed (verbal acknowledgment without a real resolution).
  - `runtime_selects_memory` (boolean, always `false`).
  - `runtime_selects_candidates` (boolean, always `false`).
  - `runtime_boundary` (string) — note that runtime surfaces the reminder; the agent judges intent presence and resolution path.

---

### `agentlaw_session_save`
Implements the Canonical Save Route in `MEMORY_AND_CONTINUITY_RULES.md`.

Usage note: call this at material session boundaries such as milestones, important decision changes, handoffs, or before stopping work. Do not use it as a frequent heartbeat after small edits or routine verification.

**Parameters**

All working-set fields below carry the **current snapshot only** — each entry is one or two short lines. Substantive session narrative (Finding/Evidence/Resolution prose, decision rationale, change history) does not belong in working-set fields; it belongs in the `log_entry` body. See `docs/law/MEMORY_AND_CONTINUITY_RULES.md` §Working Set Field Discipline for the full rule and concrete examples.

- `current_goal` (string, required) — one or two sentences describing the session's working goal. No paragraph-length narrative.
- `current_decisions` (array of strings, optional) — short list of standing decisions. Each entry is one short sentence stating a current state, not the rationale path that produced it.
- `open_questions` (array of strings, optional) — short list of open issues. Each entry one short sentence.
- `next_actions` (array of strings, optional) — short list of next steps. Each entry one short sentence.
- `authority_warnings` (array of strings, optional) — short list of standing warnings. Each entry one short sentence.
- `log_entry` (object, optional) — `{ kind, title, body, tags? }`. When provided, appended to today's log file via `memory_append_log` semantics. **Substantive session narrative belongs here, not in the working-set fields above.**
- `investigation_log_gap` (object, optional) — agent-supplied turn-level signal for the §Read Routing Criterion / §Log Write Criterion review. Recommended shape: `{ turns_with_substantive_reads, turns_with_reads_but_zero_writes, flagged_turns: [string] }`. The MCP server cannot observe turn boundaries itself; when omitted the response returns a stub with a note. See `docs/law/MEMORY_AND_CONTINUITY_RULES.md` §Write Discipline for what counts as a "flagged turn".

**Returns**
- `working_set_updated_at`.
- `log_entry_id` (when log_entry was provided).
- `save_status` — `saved` or `saved_with_degraded_log`.
- `verification_required` — always `true` in V1. The tool surfaces the post-save verification obligation but does not run project verification commands.
- `verify_hint` — concise caller guidance for the verification command.
- `memory_write_summary` — `{ total_writes, log_writes_by_kind, item_writes_by_type, zero_write_warning, lookback_since, lookback_fallback_used }`. Counts memory writes since the previous working-set `updated_at`; falls back to a 24-hour window when no prior timestamp is readable. `zero_write_warning` is non-null when the count is zero, with a pointer to the §Log Write Criterion. Histograms are keyed by what the DB tracks (log kind, item type), not by tool name.
- `investigation_log_gap` — echoes the agent-supplied dict when provided, or returns a stub `{ turns_with_substantive_reads: null, turns_with_reads_but_zero_writes: null, flagged_turns: [], note: "..." }` when omitted.
- `promotion_reminder` (object) — save-time reminder of the agent-applied promotion protocol. The dict is forward-looking; runtime never selects promotion candidates. Sub-keys:
  - `required_when` (array) — the three properties that must all be true for a promotion proposal: `durability`, `future_operational_relevance`, `authority_gap`.
  - `required_when_explanation` (object) — one-line explanation for each property.
  - `negative_cases` (array) — examples of when NOT to propose promotion (routine progress updates, one-off instructions, etc.).
  - `agent_action` (string) — the call to make when criteria are met.
  - `source_id_rule` (string) — id provenance — only ids returned by other memory tools may be used.
  - `source_ids_available` (object) — any source ids surfaced by this save (typically `{log_entry_id: "..."}` when `log_entry` was provided).
  - `runtime_selects_candidates` (boolean, always `false`).
  - `runtime_boundary` (string).

- `memory_intent_reminder` (object) — save-time reminder of the Memory Intent Rule. Same schema as the `memory_intent_reminder` field on the `agentlaw_session_restore` packet (see Read Tools section above).
- `plan_discipline_reminder` (object) — save-time planning workflow reminder. Same schema as the `plan_discipline_reminder` field on the `agentlaw_session_restore` packet (see Read Tools section above).
- `degraded` and `log_error` when the working set was written but a requested log append failed.
- `runtime_status`.

---

## Operational Tools

### `memory_runtime_check`
Preview memory runtime repair from canonical Markdown without mutating runtime state.

**Parameters**
- `scope` (string, optional, default `all`) — `all` / `items` / `logs`.

**Returns**
- `items_indexed`, `logs_indexed`, `chunks_generated`, `embeddings_generated`.
- `check_only` — always `true`.
- `duration_ms`.
- `runtime_status`.

Use before repair when source drift is suspected and the agent needs counts/warnings only. This operation parses canonical Markdown and reports what repair would process, but does not create a job row or mutate DB, FTS, vector, or embedding metadata tables.

---

### `memory_runtime_repair`
Repair derived DB/chunk/FTS state and item vector rows from canonical Markdown.

**Parameters**
- `scope` (string, optional, default `all`) — `all` / `items` / `logs`.

**Returns**
- `items_indexed`, `logs_indexed`, `chunks_generated`, `embeddings_generated`.
- `check_only` — always `false`.
- `job_id`, `job_status`.
- `duration_ms`.
- `runtime_status`.

**Conflict**
- When another runtime repair job is `queued` or `running`, returns `error.code = memory.runtime_repair_conflict` with `active_job`, `active_job_id`, `active_status`, `active_scope`, and `status_tool = memory_runtime_repair_status`. The caller should poll the active job instead of starting another repair.

Use after schema migration, after embedding model swap, or when source drift is detected. This operation does not rebuild canonical memory; it repairs derived DB, FTS, and item vector state from canonical Markdown. Log vector rows are not generated in v1; log/all repair removes accidental log vector rows while preserving log records and log chunks. Runtime repair is globally single-flight, and source Markdown is parsed only after the repair job guard is acquired.

---

### `memory_runtime_repair_start`
Start a memory runtime repair job and return immediately.

**Parameters**
- `scope` (string, optional, default `all`) — `all` / `items` / `logs`.

**Returns**
- `job_id`.
- `status` — initially `queued`.
- `operation` — `runtime_repair`.
- `scope`.
- `status_tool` — `memory_runtime_repair_status`.
- `runtime_status`.

**Conflict**
- Same `memory.runtime_repair_conflict` shape as `memory_runtime_repair`.

Use when model-backed runtime repair may exceed the agent host's MCP tool-call timeout.

---

### `memory_runtime_repair_status`
Return status and result for a memory runtime repair job.

**Parameters**
- `job_id` (string, required).

**Returns**
- `id`, `operation`, `scope`, `check_only`.
- `status` — `queued` / `running` / `succeeded` / `failed`. A queued/running job that has exceeded the stale threshold is converted to `failed` with `error_code = memory.job_stale`.
- `requested_at`, `started_at`, `finished_at`.
- `result` when succeeded.
- `error_code`, `error_details` when failed.
- `runtime_status`.

---

### `memory_health_check`
Report runtime status.

**Parameters** — none.

**Returns**
- `mcp_server` — `running` / `degraded` / `unavailable`.
- `meta_db` — `ready` / `missing` / `corrupt` / `migration_required`.
- `fts_index` — `ready` / `missing` / `rebuilding` / `stale`.
- `vector_index` — `ready` / `missing` / `rebuilding` / `stale`.
- `embedding_model` — `loaded` / `on_disk` / `incomplete` / `missing`. `loaded` means the model is in the MCP server's in-memory cache and ready for inference without a disk read. `on_disk` means the files are fully present but nothing is loaded yet (e.g. startup warmup failed or the server is in a read-only phase). `incomplete` means files exist but the set is partial — run `agentlaw init` (without `--skip-model`) to repair. `missing` means no model directory.
- `embedding_model_on_disk` — `present` / `incomplete` / `missing`. Separates the file-presence signal from the in-memory signal above, so a client can tell whether a `missing`-on-memory state is caused by missing files or a failed load.
- `schema_version` (integer).
- `kit_version` (string).
- `embedding_runtime` — object containing `model_id`, `dimension`, `status`, `last_rebuild_at`, and `last_error` for the active vector-index contract.

**Offline inference by design**: the MCP server sets `HF_HUB_OFFLINE=1`, `TRANSFORMERS_OFFLINE=1`, `HF_HUB_DOWNLOAD_TIMEOUT=5`, and `HF_HUB_DISABLE_TELEMETRY=1` at entry-point top and passes `local_files_only=True` to the embedding loader, so no Hugging Face Hub contact occurs while serving requests. The initial model download is a separate path handled by `agentlaw init`. If a manual rebuild against Hub is ever needed, set these env vars externally before invoking `run-mcp`.

---

## Open Items

- Implementation-time refinements as the MCP server is built (parameter shapes, default values, edge-case handling). Revisions go through normal governance update.
- Whether `memory_propose_promotion` should also create an entry in a dedicated proposals queue beyond the log entry (deferred until promotion review process is exercised in practice).
