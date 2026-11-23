# Agentlaw 0.5.0

- Runs one shared runtime per installation. Each harness session starts a thin
  stdio MCP frontend that attaches to the runtime over local IPC, so a new
  session reuses the already prepared stores and embedding model instead of
  repeating that preparation. MCP and CLI requests use the same runtime.
- Removes the per-session worker daemon and its shared SQLite broker database.
  Model loading no longer competes with other clients for a database lock, and
  a failed model child is cleaned up and restarted by the runtime.
- Default `doctor` reports runtime health without waiting for store locks or
  starting the runtime; `doctor --audit` performs the full audit and still
  returns health if the audit fails.
- Large recalls keep standing rules in a minimal form and reduce candidates item
  by item, reporting matched and shown counts, before falling back to a complete
  file.
- `share import prepare` reports its current stage and elapsed time on stderr,
  and imports can be listed and recovered without the lost output.
- Embedding inputs are bounded to 512 model tokens; truncated queries carry a
  `semantic_query_truncated` diagnostic.
- Installation files are replaced safely while readers hold them open, with a
  bounded retry for transient Windows locks such as antivirus scans. After a
  managed update, items still used by running processes are removed on a later
  start instead of blocking the update.
- Preserves the MCP schema and durable memory formats. A verified update from
  0.4.x removes the previous version's operating files.

Known limits: on Windows, a runtime first started by Codex ends when that Codex
process exits; the next call from any harness starts a new runtime. Standing
rules are still delivered in full, so roughly 60 or more applicable rules move a
recall into a complete file. Import preparation time itself was not reduced.
Publishing does not update an active installation. See [usage](usage.md).

## Previous release: 0.4.2

- Adds contextual help and machine-readable command descriptions. Help works
  without loading configuration, installation state or the memory store;
  internal updater handoff commands remain hidden from public help.
- Root help remains JSON, but its status is now `help` instead of
  `implementation_in_progress`, and its command list contains only public
  commands. Consumers must accommodate this content change rather than rely
  on the former status or internal command list.
- Uses typed CLI arguments with early syntax validation and bounded JSON input.
  Invalid options, duplicate JSON keys and oversized payloads fail before
  command execution, while established error codes and exit statuses are retained.
- Clarifies that the installed public `agentlaw update` owns the complete
  managed update. Successful installation requires only an ordinary harness
  restart, without redundant caller-side diagnostics; internal verification
  and incomplete-update recovery remain in place.
- Preserves the MCP schema, durable data formats and existing updater wires.
  Adds isolated regressions for help, malformed input and released-launcher
  forwarding and recovery compatibility.

This patch improves CLI usability without changing the memory tool contract.
Publishing does not update an active installation or establish semantic-model
readiness. See [usage](usage.md) for supported commands and operating limits.

## Previous release: 0.4.1

- Adds a conditional instruction to disclose incomplete semantic retrieval in
  the user's language. Direct context restoration is included; exact ID-only
  lookup and healthy retrieval gain no notice. Existing required-context
  warnings and complete recalled data are preserved.
- Keeps the instruction visible when either delivery layer moves a large
  response into a complete file. This preserves guidance delivery, not a
  guarantee that every downstream model will follow it.
- Tests and builds release files once in the PR. After merge, publication
  verifies the successful candidate run, merged source tree and exact artifact
  digests, then releases those same files without a second test/build pass.
- Includes release provenance and SHA-256 checksums. A failed publication can
  resume without overwriting existing assets or recreating a completed release.

Publishing does not update an active installation or repair semantic retrieval.
Real-model quality and downstream LLM notification behavior are separate from
the regression and release-pipeline checks.

## Previous release: 0.4.0

- Adds one typed `sync` action and CLI operation for explicit memory
  synchronization. Agentlaw owns Git execution and recovery; the agent starts
  the operation, judges conflicts when needed, and reports the result.
- Fixes the outgoing sharing cutoff. Later ordinary memory saves remain local
  for a future sync, including later changes to the same memory. Unpublished
  proposals are not shared. Ordinary `remember_this` still does not commit or push.
- Delivers complete frozen conflict packets and accepts a whole solution for
  memory, structural and dependent-reference conflicts. Runtime allocates
  identities and validates lineage, references and the resulting graph; it does
  not invent semantic resolutions.
- Records operation/request identity and checks completed effects before stale
  inputs. Status and resume handle response loss, local application, Git handoff
  and confirmed remote delivery without treating partial success as completion.
- Scans raw outgoing Git history and retains a sealed independent transfer store.
  Retries validate that store and reuse pattern findings instead of searching
  the same history again. A changed outgoing candidate requires a new scan.
- Corrects legacy import completion inspection, Windows extended Git paths,
  external Git error classification and ownership of temporary index locks.
  Unrelated staged changes and foreign index locks are preserved.

**Activation and compatibility:** Sync delegation is disabled by default. A user
must separately review and activate an OS-local policy with
`--confirm-delegation`; model calls can only select a registered policy.
Sensitive findings require a separate exact-candidate decision with
`--confirm-sharing`. Neither confirmation substitutes for the other. This local
policy is not a security boundary against tools running as the same OS user.
The existing actions remain available; consumers that enumerate tool actions
must accept the added `sync` branch. Releases through 0.3.6 do not expose it.
No active installation, policy or live memory store is changed by publishing
this release.

See [usage](usage.md#git-and-management) for activation, the operation flow and
resource bounds.
No measured speedup for the original 20-minute case is claimed. The six-pattern
scanner is not complete secret/PII classification; receipt reuse is not full
fsck or exactly-once scanning of an interrupted attempt. Real-model resolution
quality, physical power loss and every whole-sync crash combination remain
outside the verified scope.
