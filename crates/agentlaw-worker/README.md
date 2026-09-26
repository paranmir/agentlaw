# C7 durable coordinator and process runtime

Implemented: SQLite-durable jobs, full model/config/revision/content/section
single-flight key, separate client/operation leases, atomic start reservation,
worker incarnations, one running job, weighted foreground/background claim,
bounded admission, retained retry/failure records with 1–60s backoff, immutable
float32 job results, explicit index acknowledgement, idle/orphan predicates.
Time arguments and TTL/grace values use milliseconds. Worker READY is not changed
to unavailable because a queue is slow. Cancel/release removes only that lease's
waiters; durable jobs and other clients remain.

`ProcessRuntime::attach(&RuntimeConfig)` starts/reuses an OS-locked daemon with a
bounded authenticated loopback TCP protocol. The broker supervises a separate
private model-child process; client leases heartbeat every 10 seconds and expire in
60 seconds. Requests are capped at 4 MiB. Explicit cancellation removes only its
waiter; normal READY queue waits use completion notifications until cancellation or failure.
The listener uses bounded I/O deadlines, never a semantic-search time cutoff.
Idle shutdown occurs after 10 minutes without live leases or runnable jobs.
`reserve_worker_start` is also exposed as a lower-level transactional primitive.

`OnnxProvider::load(&ModelAssets)` uses the real `ort` dynamic binding and Hugging
Face tokenizer JSON. Named int64 inputs run on a single-thread CPU session;
Granite's CLS 768-dimensional output is truncated to 256 and normalized. Model and
tokenizer are hashed using bounded reads. The model, tokenizer and trusted runtime
DLL/shared-library paths must be explicitly provisioned. Nothing is downloaded.
Missing assets return typed unavailable. `UnavailableProvider` is also available.
No fake/hash embedding is substituted. Real official INT8 CPU inference and
daemon IPC have now passed local smoke tests; see [artifact and evidence log](ARTIFACTS.md).
The release pins official model/tokenizer SHA-256 values; other artifacts fail
explicitly instead of silently being labeled Granite.
Load performs one internal 64-valid-token warmup from a fixed built-in English
input and validates dimensions, finite/nonzero output and normalization before
READY. This does not truncate or pad user queries to 64 tokens. GPU selection and
measurement are implemented in the model child; GPU-device validation is pending.

Public process API:

```text
RuntimeConfig { state_dir: PathBuf, executable: PathBuf, model: Option<ModelAssets> }
ModelAssets { onnx_model: PathBuf, tokenizer_json: PathBuf, runtime_library: PathBuf }
ProcessRuntime::attach(&config) -> Result<Client>
Client::availability(), Client::embed(text), Client::embed_cancellable(text, &AtomicBool)
Client::model_digest(), Client::derived_position(context), Client::ingest_published(context, page)
run_daemon(config) -> Result<()>
run_model_child() -> Result<()> // hidden model-child route; inherited pipe launch config
inspect_runtime(state_dir) -> Result<serde_json::Value> // read-only; no attach/spawn
```

The executable must route hidden arguments
`worker-daemon --state-dir PATH [--model FILE --tokenizer FILE --ort-library DLL]`
to `run_daemon`. Windows child startup uses CREATE_NO_WINDOW. Endpoint secrets
are not logged; Unix state/endpoint modes are 0700/0600. On Windows a hidden
PowerShell call uses .NET directory DACL APIs to restrict the dedicated state
directory and inherited children to the current OS user (failure blocks startup).
The endpoint is written to a private same-directory temporary file, synced, then
renamed under the daemon lock. Hostile-local-user/race tests remain. Endpoint
state is disposable, not source. Never choose a general-purpose directory as
`state_dir`: it is a dedicated private broker directory, not the source repository.

`derived::DerivedWorkCoordinator` accepts only a read-only `PublishedSourcePort`.
The C6 adapter supplies owned immutable change pages; no source paths/writer are
given to C7. Contiguous publication batches and epoch are checked; queue insertion
and the **accepted** source cursor commit together. Overload rolls both back.
Tombstones become ready-to-index work without inference. Cursor advance is not an
index acknowledgment. The daemon IPC accepts the same verified materialized pages;
RAM notifications are unnecessary for replay from the persisted cursor.

`Client::stage_source_body` streams C6-owned content to a private opaque spool;
`ingest_spooled` sends only verified descriptors. No canonical path is accepted.
The coordinator splits UTF-8 into bounded sections, commits publication membership
and queue acceptance together, and keeps source acceptance separate from index ack.
Lexical and vector sinks stream durable rows into separate backend transactions;
only the verified immutable commit receipt advances that channel's watermark.
`search_index[_cancellable]` waits for requested READY-vector coverage and returns
lexical/BM25, lexical-strength, vector hits, and pinned backend stamps. Superseded
failed revisions do not block latest-head coverage. Durable source backlog does
not occupy foreground query admission slots.

`repair_index` rebuilds a separate verified generation from the accepted ledger,
publishes its pointer atomically, and retains old files/readers. Missing ledger
coverage or an unavailable backend is an explicit failure, never empty success.
`ready_index_batch`/`acknowledge_index` expose the bounded handoff API; the daemon
uses streaming handoff for publications exceeding a bounded materialized batch.

Fresh-clone inventory uses `Client::bootstrap_status(context)` and
`bootstrap_spooled(context, page_number, final_page, page)`. Numbered inventory
pages stay at the actual pinned initial source fence (including sequence zero);
they do not invent source publications. Page receipts, queued sections and next
page are durable together. The frontend retains its inventory manifest until
finalization. Exact lost-reply replays work after accepted ingress spools are
reclaimed. Separate lexical/vector bootstrap flags only advance after actual
backend commits; an empty sequence-zero watermark never substitutes for that
acknowledgement. Normal published changes cannot overtake unfinished inventory.
Bootstrap/restart/gap/replay and zero-fence generation-repair tests cover this path.

Remaining validation: adversarial IPC/power-loss fault injection, explicit disk
reservation accounting, representative whole-process-tree memory/latency and
ANN-recall quality measurements, GPU-device execution and platform matrix tests.
Durable jobs and publication pages use Broker and private IPC.
Failed work stays available for explicit
retry. Daemon loading/thread failures are retained in its diagnostics table and
reported explicitly. Query result caches retain at most 1024 unowned disposable
rows; successful query payloads are cleared. This does not delete durable jobs.

TCP_NODELAY, blocking accept, condition-variable job/completion notifications and
long-wait responses replace inference/result polling. Socket read timeout slices
only observe the legacy AtomicBool cancellation flag; they do not send result polls.
Retry waits use the next due timer (1s exponential to 60s, eight-attempt retention);
worker recovery resumes only claims lost with that incarnation; it does not reset
unrelated permanent job failures. Existing Clients reconnect to
new daemon incarnations without changing the request's operation lease or job ID.
Four heavy permits and 64 bounded handlers guard bursts. Realistic load validation
is still required; this is not an ultra-low-latency SLA claim.

A-11.1 implementation: a serialized broker supervisor owns the exact child handle,
private pipe execution protocol, independent authenticated control, worker
incarnation and claim attempt. Native exit is observed with a duplicated Windows
process handle (`WaitForSingleObject`) or Linux/macOS `libc::waitid(WNOWAIT)` with
platform-native constants and `siginfo_t`; no PID-only kill
or inference-duration timeout is used. A transient control reconnect keeps the
same model. Irrecoverable execution-pipe failure stops only the owned child and
waits for confirmed termination before replacement. Current-config leases and
durable work drive automatic recovery, not a subsequent MCP call. Lost foreground
waiters retain one failure while durable claims resume; valid results/ACKs remain.
The persisted worker budget is 1s, 2s, then 5-minute cooldown, reset only after 60s
READY/control stability. Confirmed asset errors block repeated loading until asset
metadata changes. Model-lock ownership prevents overlap with an orphan after broker
death; the child self-exits after 60s without authenticated broker heartbeats.
Idle stops unload the model while the broker remains available to serialize new
demand. Doctor is read-only and reports failures, recovery, queue/fences and separate
broker/model/combined RSS; unknown measurements remain unknown.

This supervisor/process-isolation addition is **compile-checked, not runtime-tested**:
the user explicitly deferred tests during this implementation phase. Earlier
real-model/IPC evidence below predates this isolation change. DD-T16-a through i,
native child crash recovery and platform execution still require execution evidence.
Linux and macOS exit adapters are implemented but have not been target-compiled or
executed in this Windows session; Windows `cargo check` is not evidence of either.

Validation: standard worker tests pass on Windows gnullvm, including real binding compile,
missing-artifact failure, disk reopen, cross-connection start reservation, queue
single-flight/backoff, capacity/failed retention, old result rejection and leases.
The real child-process IPC test starts a daemon, attaches two clients, checks
reuse and independent lease release, and verifies missing assets are unavailable.
The process test also covers existing-client crash recovery, a 6 MiB body spool,
corrupt-derived-index failure and generation repair. A provisioned-artifact test
(normally ignored) passed actual portable-QDQ model IPC, publication embedding,
vector commit/ack, semantic recall, and cancellation while waiting for a fence.

CPU becomes READY after the fixed 64-valid-token warmup. A background candidate
uses CUDA or DirectML only if the provisioned runtime exposes it; three alternating
timings select GPU only when its full range is below CPU's range. Fingerprints
include model build, runtime bytes, CPU, GPU identity and driver. Cached selections
are revalidated by warmup, and the unused session is dropped. Host resident bytes
are observed; unsupported dedicated/shared GPU observations are `None`, never zero.
Actual GPU execution is not verified by the CPU-only library used in local tests.

Pooling reference: [IBM model configuration](https://huggingface.co/ibm-granite/granite-embedding-311m-multilingual-r2/raw/main/1_Pooling/config.json).
