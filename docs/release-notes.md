# Agentlaw 0.4.0

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

See [usage](usage.md#git-and-management) for activation and the operation flow,
and [verification](verification.md) for actual checks and resource bounds.
No measured speedup for the original 20-minute case is claimed. The six-pattern
scanner is not complete secret/PII classification; receipt reuse is not full
fsck or exactly-once scanning of an interrupted attempt. Real-model resolution
quality, physical power loss and every whole-sync crash combination remain
outside the verified scope.
