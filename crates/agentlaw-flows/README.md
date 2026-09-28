# Agentlaw context, recall and write flows

Concrete Runtime connects C2/C3/C5 workflows to canonical storage, SQLite local
control, incremental metadata/lexical indexes and optional C7. Pure port defaults
remain test boundaries, not substitutes for concrete adapters.

Implemented: explicit project association and durable catalog creation; installation
machine identity injection; disk-backed exact membership and incremental source-path
replay; indexed candidate previews followed by exact source/required-closure reads;
one-hop related discovery; common BM25/vector/RRF memory and procedure search
(procedure management defaults to all scopes); admitted automatic full selection;
complete active Task handoff sections; durable pending delta/CAS and interrupted
procedure inspection/revision; read-set publication and predecision cancellation.

Background source pumping sends authenticated worker-owned bounded body spools to
durable C7 jobs. Query embedding does not rebuild the corpus. Derived contexts include
source epoch. Connection preparation waits for configured semantic builds, while
private-generation repair preserves prior indexes and pending work.

Oversized exact and discovery-selected reads stream complete acquired bodies to version-frozen JSON files.
The original response template retains rules, required closure, candidates/counts,
and complete Task sections. Initial C7 inventory uses a durable transfer manifest
before source-ledger replay, including a restored store at source sequence zero.
Background pump failures are retained for recall/doctor and cleared after recovery.
An initialization marker prevents silently replacing lost authoritative control
state. Connect-and-restore preserves file delivery and incomplete-provider status.
Write review uses target-scope whole/section admission, relevant dependent review
keys, explicit diagnostics, and one result per proposal even for consolidation.
Artifacts report seven-day retention and bounded registry-only cleanup. Cleanup
never targets source/pending files. History uses the separate disk-backed projector,
whose additional verification is maintained by the integration owner.

Verified on Windows: 19 runtime journeys (pending/replay, cancellation, procedure
interruption, consolidation, scope isolation, Task compact, read-set concurrency,
private-generation repair); the >64 MiB exact-delivery fixture; two artifact
expiry/isolation tests; three worker spool tests including failure cleanup.
Contract fixture and pure-flow tests are separate.

Remaining limitations, not completion claims:

- Latest initial-inventory integration is compile-checked only: further tests were
  paused at user request. Subsequent binding-marker, background-status, review-key,
  procedure-neighbor and pending-notice audit fixes are compile-checked only.
  The >64 MiB discovery-rule and streamed Task-section tests
  passed before that instruction. Ordinary candidate previews are bounded.
- The 10,000-candidate ceiling reports incomplete coverage. ANN candidate counts
  are not global semantic matched counts. Overlap corpus watches conservatively
  invalidate on any memory publication.
- Real-model admission quality, large-corpus latency and exhaustive crash/power-loss
  matrices need more evidence than these tests.
- Corrupt control SQLite cannot safely be recreated from canonical source:
  uncommitted pending/authoring payloads are authoritative there and require
  preservation/recovery. Derived indexes are independently rebuildable.

This crate does not edit project files, create commits, or transport data through Git.
