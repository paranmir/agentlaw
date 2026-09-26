# C4 derived disk search

Implemented: disk-backed SQLite inverted postings with BM25 (`k1=1.2,b=0.75`),
scope filtering, immutable WAL reader transactions with matching commit/ack stamps,
complete-delta overlay invalidation (including tombstones), and deterministic
memory-ID RRF (`k=60`). An exact disk vector backend stores float32 BLOBs,
validates model/config/dimensions, scans bounded records, aggregates maximum
cosine per memory and retains only bounded top-k candidates. This exact scanner
remains an oracle; the daemon uses the incremental disk multi-probe ANN below.

`SearchIndex::commit` takes a complete change batch through the supplied watermark.
Caller must acquire canonical data through C6 and verify contiguous delta coverage;
this crate never reads/writes source files. A `ReadView` pins its backend commit and
ack together. `search(..., delta_complete=false)` fails explicitly, never returns
an empty success. Overlay arguments must include latest changes and removals even
when new content has no query term. Vector overlay must include every pending
in-scope embedding, not merely lexical matches. Use bounded ingestion batches.

Scopes are opaque exact tokens (`user`, `machine:<id>`, `project:<id>`,
`project:<id>:machine:<id>`); callers supply all allowed tokens. Lexical documents
may represent multiple current heads with distinct scopes; `(memory_id,change_id)`
is unique. Scope-filtered records are aggregated to the highest score per memory,
so multiple heads do not consume result slots. A changed identity invalidates all
old head records; overlay/commit must supply its full latest head set. Full bodies
are resolved by C6, not from the index. The old single-head derived schema is
explicitly rejected for rebuild, never migrated by modifying source.

`commit_stream` accepts ordered, bounded head chunks and a changed-ID iterator.
It consumes one chunk at a time, retains an incremental normalized-token SHA-256
state across boundaries (even an exceptionally long token), and commits one
source watermark only after the complete stream. Per-scope corpus statistics
have a bounded 128-profile cache updated using only changed identities; first
use of a new profile counts disk metadata once. `lexical_strength` implements
IDF-weighted query-term coverage, with zero information mass scoring zero.
Schema v3 stores each 32-byte normalized-token digest once in `lexicon`; postings
use short numeric term/document IDs and SQLite integer varints in a WITHOUT ROWID
B-tree. A 5,000-document structural fixture verifies five dictionary entries for
25,000 numeric postings. This removes repeated 64-character digests but is not
custom delta-compressed posting blocks, skip lists, or a measured million-record
capacity guarantee. Prior analyzer generations require derived rebuild.

`VectorReadView::search_ann` uses 12 deterministic random-hyperplane tables,
12 bits, Hamming-radius-one disk bucket probes, and exact cosine re-scoring.
Scope filtering precedes the candidate bound (4096 or 8 times requested top-k).
This is explicitly approximate retrieval, not an exact global nearest-neighbor
claim. No query-time full vector scan/rebuild occurs in the daemon path.
`AnnMetrics` exposes evaluated candidate counts. Recall-quality calibration of
this reversible profile on a representative real corpus remains necessary.

`generations::GenerationCatalog` builds isolated directories, verifies backend
stamps, atomically publishes an active pointer, and retains retired files until
cross-process reader leases are released. Old lexical analyzer/ANN schemas fail
explicitly and can be rebuilt from C7's accepted durable publication ledger.

Validation: scope/overlay/multihead tests, a 10,000-vector incremental disk fixture
with bounded candidate evaluation, incremental scope-statistics equivalence,
and generation-swap/reader-retention tests pass. This is structural evidence,
not a million-record latency, memory, ANN-quality, or cross-platform benchmark.
Model inference belongs to C7; no synthetic production embeddings are used.
