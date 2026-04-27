# harness-kit

A law-first governance kit for AI coding agents — install governance structure before writing code.

`harness-kit` gives any repository a governed starting structure that AI coding agents can read, follow, and maintain. It works for brand-new repositories and already-existing codebases alike. Drop it in, run the bootstrap, and the agent knows what the rules are before it writes a single line.

---

## For Humans

### Install

```bash
pipx install harness-kit
```

### Quick Start

```bash
# 1. Place harness-kit governance into your project
harness-kit init <your-project-dir>

# 2. Register the harness-memory MCP server with your AI agent host.
#    The default `--setup-agents prompt` mode emits LLM-actionable
#    instructions — copy them into your agent and let it edit its host
#    config. Restart the host after the edit lands.

# 3. Open a fresh chat with your AI agent on the project and say:
#       Restore the session
#    The agent loads the harness context and you can start work.
```

### Using harness-kit in Your AI Coding Session

Once harness-kit is initialized in your project and the `harness-memory` MCP server is registered with your agent host, you drive the harness through natural-language triggers in your conversation. The agent maps each trigger to one of the harness's MCP tools and follows the procedure that the kit ships.

**Triggering session restore** — at the start of any new conversation on the project, say one of:

- "Restore the session" / "세션 복원해봐"
- "Pick up where we left off"
- "지금 어디까지 했어?"

The agent calls `harness_session_restore`, the response packet carries your project's working set, every active plan body, the most recent session_save log entry, framework reminders (memory intent rule, write discipline, consult-before-answer rule), and a step-by-step reminder of the §Canonical Restore Route Mandatory Tier the agent must follow before answering. The first turn of every session is a context-loading turn; substantive work starts on turn two.

**Triggering session save** — when ending a session, before context compaction, or at any milestone, say one of:

- "Save the session" / "세션 저장해줘"
- "Wrap up and save state"
- "Snapshot what we did"

The agent calls `harness_session_save` with the working frame, and the save tool surfaces a post-save verification obligation that you run after.

**When something feels off** — if the agent appears to be answering with stale context or missing rules, ask it to call `harness_session_restore` again, or run `harness-kit mcp-recover --target . --client auto --json` to diagnose MCP connectivity from the shell side.

### What This Project Is

**The problem.** AI coding agents arrive at a repository without governance. They make plausible-looking changes that violate invariants the team has not written down. The team adds a CLAUDE.md or AGENTS.md to capture rules; the file grows; the agent reads less of it; the rules silently stop applying. The agent and the team need shared structure they can both rely on.

**The kit.** harness-kit installs that structure as governance scaffolding: a constitution, a law layer (memory, artifact, oracle, failure rules), root control tools (init / update / fix), contract documents, a memory subsystem (working set, logs, rules, preferences), and a runtime MCP server that surfaces the rules every session. The agent reads the law before it writes; the kit's verifier mechanically catches drift between the rules and the code.

**Recursive improvement.** The kit develops by using itself. Every plan that lands in the source repository goes through the same `HARNESS_INIT_TOOL.md` / `HARNESS_UPDATE_TOOL.md` / `HARNESS_FIX_TOOL.md` rules the kit ships. Every law change is mirrored to the public seed (this repository) and to the bundled package scaffold. The same `harness-kit verify` you run against your project also runs against the kit itself.

### Requirements

- **Python** 3.11 or newer.
- **Operating systems**: Windows, Ubuntu, and macOS supported.
- **Disk**: the embedding model occupies roughly 500 MB once downloaded (cached under `<your-project>/.harness/models/`).
- **Runtime dependencies** are installed automatically by `pipx install harness-kit` and declared in `pyproject.toml`.

### What You Get After `harness-kit init`

```
<your-project>/
├── HARNESS_CONSTITUTION.md         # highest authority
├── HARNESS_INIT_TOOL.md            # bootstrap entry
├── HARNESS_UPDATE_TOOL.md          # update flow
├── HARNESS_FIX_TOOL.md             # gap-fix flow
├── AGENTS.md                       # routing-only entry map for agents
├── docs/
│   ├── harness/                    # law layer
│   ├── contracts/                  # boundary contracts
│   └── references/                 # local references
├── memory/                         # working-set, logs, rules, preferences
├── docs/plans/                     # plans (initially empty)
└── .harness/                       # runtime tree (DB, embedding model)
```

The agent reads law and routing on every session restore; you do not maintain a separate CLAUDE.md / AGENTS.md / .cursorrules.

### Links

- **PyPI**: https://pypi.org/project/harness-kit/ — for source repository and issue tracker, follow the links on the PyPI project page.

---

## For AI Agents

This section is the agent-facing reading order for first-time entry into a project that uses harness-kit. If you are reading this README as a new session, follow the §Canonical Restore Route Mandatory Tier in `docs/harness/MEMORY_AND_CONTINUITY_RULES.md` instead — that procedure is the binding one. This README is the on-ramp; the law file is the substance.

### Agent Bootstrap (first install)

When the user asks you to set up harness-kit in a project:

1. Install harness-kit: `pipx install harness-kit`.
2. Run `harness-kit init <target-project> --setup-agents prompt` in the target. The `prompt` default emits LLM-actionable instructions for registering the `harness-memory` MCP server in your host config; read those instructions and edit your host config rather than letting init auto-write to user-level config (that requires `--setup-agents auto --yes`).
3. Restart your host after the config edit so the new MCP registration is picked up.
4. On the next agent session, call `harness_session_restore` (MCP) or `harness-kit session-restore --target . --json` (CLI fallback) and follow the §Canonical Restore Route Mandatory Tier in the response.
5. If the MCP server is not visible in a new session despite restart, run `harness-kit mcp-recover --target . --client auto --json` to diagnose runtime + registration state.

### Layer-by-layer map (what each artifact class is for)

- **`HARNESS_CONSTITUTION.md`** — highest authority; structural invariants. Rare changes; never violate.
- **Root control tools** (`HARNESS_INIT_TOOL.md`, `HARNESS_UPDATE_TOOL.md`, `HARNESS_FIX_TOOL.md`) — agent-facing procedure documents. Init bootstraps a fresh project, Update incorporates kit upgrades into an existing target, Fix runs the gap-resolution protocol.
- **`docs/harness/*` (law layer)** — rules every session reads: memory and continuity, artifact rules, oracle and judgment, code authorship and stewardship, failure taxonomy, mechanical enforcement policy, starter specialization rules, scope, input/output contract.
- **`docs/contracts/*`** — boundary surfaces shared with the kit (MCP tool surface, shared baseline, update workflow).
- **`docs/references/*`** — research-and-context references; not authoritative.
- **`memory/*`** — derived continuity (working-set, logs, rules, preferences, lookup rules). Below law in authority.
- **`docs/plans/active/*` and `docs/plans/completed/*`** — work-in-flight and historical work. Active plans are read-on-restore.

### Governing hierarchy

Authority flows top-down:

1. `HARNESS_CONSTITUTION.md`
2. Root control tools
3. `docs/harness/*` (law)
4. `docs/contracts/*`
5. `docs/references/*` (non-authoritative)
6. `memory/*`
7. `AGENTS.md` (routing only — never a rule store)

When two artifacts seem to conflict, the higher one wins. Memory never overrides law; references never override contracts; AGENTS.md is the entry map, not a source of rules.

### Restore procedure on every session start

§Canonical Restore Route Mandatory Tier (full body in `docs/harness/MEMORY_AND_CONTINUITY_RULES.md`) requires 14 steps before composing a substantive response. Summary: confirm runtime integrity, read the working set, read every law file, read every active plan body, read the most recent session_save log entry, scan recent_logs titles, read every active rule's body, read `memory/preferences.md`, read `memory/LOOKUP_RULES.md`, scan the known-facts manifest, run a working-frame `memory_search` over `current_goal + next_actions + open_questions`, inspect governance drift, assemble the packet, surface gaps to the user. The runtime pre-fetches body fields into the restore packet so the substance is in your context without extra `Read` calls; the procedure is binding regardless.

### Critical rules — quick reference

One-line restatements; the binding text lives at the anchors.

- **Memory Intent Rule** — when the user expresses intent to remember, persist, or carry forward, resolve to one of `memory_write` / `promotion_proposal` / `associative_marker` / `explicit_non_save` before final response. Anchor: `docs/harness/MEMORY_AND_CONTINUITY_RULES.md` §Memory Intent Rule.
- **Write Discipline** — silence is a valid answer; a write must clear the §Log Write Criterion three-question gate (and the §Item Write Criterion applicability gate for items). Volume is not the target; selectivity is. Anchor: §Write Discipline.
- **Read Routing Criterion** — classify the question (prior judgment / cross-session / current source) before reaching for `memory_search` vs `Grep`/`Read`. Anchor: §Read Routing Criterion.
- **Consult-Before-Answer Rule** — for memory-routed questions, consult memory **before** composing the answer, not after. Anchor: §Consult-Before-Answer Rule.
- **Self-Narration Prohibition** — governed artifact bodies and code comments describe current state only; revision history lives in plans, tracker entries, memory logs, and git, not in the body. Anchor: `docs/harness/REPOSITORY_ARTIFACT_RULES.md` §Self-Narration Prohibition (paired with §Reasoning-Critical Inline Comments in `CODE_AUTHORSHIP_AND_STEWARDSHIP_RULES.md`).
- **Promotion Proposal Protocol** — runtime never selects promotion candidates; the agent judges whether durability + future operational relevance + authority gap all hold, then calls `memory_propose_promotion`. Anchor: §Promotion Proposal Protocol.

---

## License

MIT.
