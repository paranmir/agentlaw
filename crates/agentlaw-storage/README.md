# Canonical storage implementation status

Implemented: length-framed Markdown with exact UTF-8 bytes/checksums; canonical
av1 versions (including the specification's golden vector); per-unit direct
current reads; validated scope/live/redirect boundaries; all-head conditional
batch writes; same-execution retries; consolidation parent ownership; bounded
16 MiB history packs and UTF-8 chunks; checkpoint/delta verification and causal
history closure; catalog publication; dirty fence + durable decision + persistent
after-images + redo; exact reserved-trailer history append recovery; SQLite
publication ledger; indexed immutable-change admission registry; OS cooperative
lock; and a read-only C7 change reader.

`current`, `history`, `publication`, and `recovery` share the same Store gate and
persistence helpers. The C7 reader cannot publish. Its BLOB(8), big-endian indexed
sequence query requires complete coverage and an unchanged source epoch. Returned
units are latest units at the returned source position, deduplicated within a
page, not a historical snapshot for each sequence. Journal loss does not prevent
current reads; it does prevent an empty-success change stream.

The installation's `state/source-coordination` registry pins an exact
canonicalized source path to one local control directory. A second local control
directory fails closed. Registry bindings deliberately do not auto-expire:
relocation/rebinding/reuse of an old path needs an explicit recovery workflow.
The app explicitly supplies this registry to normal, read-only, repair, and Git
staging paths. Storage does not inspect profile environment variables to choose
another registry. Convenience library constructors use a sibling of their local
control directory, keeping isolated tests inside test storage. The registry is
not part of Git-shared Markdown memory. Older AppData
registries require an explicit offline migration before the new location is used
for an existing source; changing this code does not move existing bindings.

## Verification boundary

Development sync support adds bounded canonical capture, isolated historical-ref
resolution, connected read-set/predicate guards and scoped C6 overlay publication.
The final gate does not re-read entire history: new immutable frames go into
independent packs, preserving later appends to existing shards. Reverse relation,
evidence, redirect and project stamps catch newly added dependents and ABA changes;
known reviewed context paths are guarded too. Ordinary publication maintains this
local registry once bootstrapped, without Git commits or MVCC snapshots.
Sync uses the existing decision/redo journal and checks receipt/request identity
before stale inputs; an operation ID cannot name another sync plan.
Capture is capped at 100,000 files/512 MiB/cooperative 5s; final read-set at 256
nodes/paths and 16 MiB. These are explicit fail-closed prototype budgets, not
measured large-corpus performance or hard OS I/O deadlines.

Tests exercise real Windows file replacement and a subprocess abort after a
durable decision, in addition to stage-by-stage fault injection. They cover
embedded framing markers, the golden digest/version, corruption, all-head stale
checks, same-execution deduplication, torn reserved history suffix redo, journal
loss, multi-identity consolidation, cross-pack UTF-8 chunks, exact history reads,
and conflicting local source bindings. These are process-crash tests, not power
cut, device cache, synchronized-folder, remote-filesystem, or throughput tests.

Every receipt reports `FileSyncedProcessCrashProtocolPowerLossUnverified`.
Windows namespace persistence has not been established. There is no claim that
function names or SQLite commits make a multi-file batch power-loss-safe.

Publication admission serializes cooperating source operations, computes checked
working-memory/recovery/source temporary-byte estimates, queries physical memory
and source/local available space, and records the measured budget before preparing
new decisions. A final disk check precedes decision. `InsufficientResource` and
`ResourceUnknown` are distinct; already-decided redo ignores artificial limits.
This is a scoped cooperative reservation/check, not an OS guarantee against other
processes or independent bindings consuming the same volume after measurement.

## Remaining production work

- Owned convenience APIs retain a 64 MiB RAM guard. `acquire_current`,
  `acquire_closure`, `spool_history`, and `visit_history_closure` provide complete
  body bytes via immutable local spools. History DAG sorting, chunk verification,
  delta replay and current-to-history validation are disk-backed. A >64 MiB
  current-read fixture passes; million-unit throughput remains unmeasured.
- Initial attachment now performs cross-pack DAG/checkpoint/current consistency
  audit and leaves a fail-closed validation marker if it fails. `open_read_only`
  does not bootstrap, migrate, recover, or change canonical files.
- Explicit recovery-image GC preserves durable manifests/decisions/receipts.
  Complete retained decisions can reconstruct missing ledger rows without
  overwriting existing conflicting data. Missing retained coverage and a corrupt
  SQLite database still require further operator recovery; rebinding remains open.
- Automatic maintenance runs at 64-generation checkpoints, retains at least 64
  publication generations of recovery images, and removes older derived history
  caches only when their shared acquisition leases can be exclusively locked.
  Pending decisions, canonical files and permanent receipts are never GC targets.
- Unchanged history positions reuse a validated disk cache and expose causal
  metadata-only visitors. A new generation currently requires a fresh history
  acquisition; incremental immutable-frame ingestion remains a scale optimization.
- Fine-grained final read sets fence versions, absence and reverse-required
  adjacency using covered ledger ranges. Overlap scope predicates conservatively
  invalidate on any memory change because prior scopes are not in the ledger.
- There is no full checkpoint/delta compression policy or optimized incremental
  historical locator. The deterministic prefix/suffix splice encoder is lossless
  but is not a minimal line-diff implementation.
- The immutable-change admission registry is rebuilt by a bounded pack scan after
  journal loss or initial attachment. This first scan is correctness-oriented;
  million-unit cold-start throughput and online rebuild scheduling are unmeasured.
- Imported review workspaces union immutable history and retain concurrent
  maximal heads without selecting meaning. Approved resolved snapshots publish
  through the normal durable C6 decision/redo protocol, verifying old heads remain
  ancestors/retained and history is not discarded. Redirect/catalog structural
  conflicts retain both exact sidecars and require explicit local/incoming choices
  in the isolated workspace. Choices never authorize resurrection or discarded
  consolidation ancestry; staged evolve/consolidate can reconcile those lineages.
  The resolver reports the exact remaining action before user-confirmed publication.
- Corrupt C6 journals can be explicitly rebuilt only from complete bound durable
  decisions and receipts. Byte-exact original database/sidecar backups and pending
  recovery material are retained; an interrupted replacement resumes its exact
  recorded candidate. Missing authoritative coverage is an actionable error, not
  a reset. The separate unpublished proposal database cannot be reconstructed
  from canonical source and is never replaced by this operation.
- The latest structural-choice and lineage-resolver changes were compile-checked
  only; further test execution was deferred at the user's request.
- Power-loss/platform qualification and million-unit performance remain unverified.

No embedding completion or Git action is part of canonical publication success.

C8 Git capture uses a private index. The durable handoff refuses already staged
canonical changes, preserves unrelated staged entries, checks the active index
digest and HEAD, then installs only the approved canonical entries after ref CAS.
Explicit import confirmation binds a resolution token; retained handoff material
supports retries without republishing the source or pushing a remote.
