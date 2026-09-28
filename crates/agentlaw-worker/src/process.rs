//! Authenticated bounded loopback protocol. The daemon owns the model, not a frontend.
use crate::derived::{
    DerivedContext, DerivedWorkCoordinator, PublishedPage, PublishedSourcePort, SourcePosition,
};
use crate::{Admission, Broker, EmbeddingError, JobKey, ModelAssets, SemanticAvailability};
use fs2::FileExt;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const PROTOCOL_VERSION: u32 = 2;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub state_dir: PathBuf,
    pub executable: PathBuf,
    pub model: Option<ModelAssets>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Endpoint {
    protocol_version: u32,
    incarnation: String,
    address: String,
    secret: String,
    config: String,
}
#[derive(Serialize, Deserialize)]
struct Request {
    protocol_version: u32,
    incarnation: String,
    secret: String,
    lease: String,
    operation: Operation,
}
#[derive(Clone, Serialize, Deserialize)]
enum Operation {
    BootstrapStatus {
        context: DerivedContext,
    },
    BootstrapSpool {
        context: DerivedContext,
        page_number: u64,
        final_page: bool,
        page: crate::spool::PublishedSpoolPage,
    },
    RepairIndex {
        context: DerivedContext,
    },
    DerivedSpool {
        context: DerivedContext,
        page: crate::spool::PublishedSpoolPage,
    },
    ExecutionDiagnostics,
    Attach,
    Heartbeat,
    Model,
    Detach,
    Submit {
        input: String,
    },
    Poll {
        id: i64,
    },
    Cancel {
        id: i64,
    },
    DerivedPosition {
        context: DerivedContext,
    },
    DerivedSubmit {
        context: DerivedContext,
        page: PublishedPage,
    },
    Search {
        context: DerivedContext,
        query: String,
        scopes: Vec<String>,
        limit: usize,
        vector: Option<Vec<f32>>,
        required: SourcePosition,
    },
    IndexBatch {
        context: DerivedContext,
        channel: crate::indexing::Channel,
        limit: usize,
    },
    IndexAck {
        context: DerivedContext,
        receipt: crate::indexing::IndexReceipt,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchPacket {
    pub lexical_matched: u64,
    pub vector_candidates: u64,
    pub lexical: Vec<agentlaw_search::Hit>,
    pub lexical_strength: Vec<agentlaw_search::Hit>,
    pub vector: Vec<agentlaw_search::Hit>,
    pub lexical_stamp: agentlaw_search::ViewStamp,
    pub vector_stamp: Option<agentlaw_search::ViewStamp>,
    pub semantic_complete: bool,
    pub source_position: SourcePosition,
}
#[derive(Serialize, Deserialize)]
enum Response {
    BootstrapStatus {
        next_page: u64,
        complete: bool,
    },
    RepairIndex(crate::indexing::RepairReceipt),
    ExecutionDiagnostics {
        host_resident_bytes: Option<u64>,
        selection: Option<crate::execution::Selection>,
        resources: RuntimeResources,
    },
    Ok,
    State(SemanticAvailability),
    Model(String),
    Job(i64),
    Vector(Vec<f32>),
    Pending,
    Error(EmbeddingError),
    Position(SourcePosition),
    Search(SearchPacket),
    IndexBatch(Option<crate::indexing::IndexBatch>),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeResources {
    pub broker_resident_bytes: Option<u64>,
    pub model_resident_bytes: Option<u64>,
    pub combined_resident_bytes: Option<u64>,
}
#[derive(Default)]
pub(crate) struct Signal {
    version: Mutex<u64>,
    changed: Condvar,
    heavy: Mutex<usize>,
    permit: Condvar,
    request_bytes: Mutex<usize>,
}
impl Signal {
    fn reserve(&self, bytes: usize) -> Result<RequestMemory<'_>> {
        let mut used = self.request_bytes.lock().unwrap();
        if bytes > 64 * 1024 * 1024 - used.min(64 * 1024 * 1024) {
            return Err(
                EmbeddingError::Retryable("broker request memory admission full".into()).into(),
            );
        }
        *used += bytes;
        Ok(RequestMemory {
            signal: self,
            bytes,
        })
    }
    fn acquire(&self) -> HeavyPermit<'_> {
        let mut count = self.heavy.lock().unwrap();
        while *count >= 4 {
            count = self.permit.wait(count).unwrap();
        }
        *count += 1;
        HeavyPermit(self)
    }
    pub(crate) fn version(&self) -> u64 {
        *self.version.lock().unwrap()
    }
    pub(crate) fn notify(&self) {
        let mut v = self.version.lock().unwrap();
        *v = v.wrapping_add(1);
        self.changed.notify_all();
    }
    pub(crate) fn wait(&self, version: u64, timeout: Duration) {
        let guard = self.version.lock().unwrap();
        let _ = self
            .changed
            .wait_timeout_while(guard, timeout, |v| *v == version)
            .unwrap();
    }
}
pub struct ProcessRuntime;
struct HeavyPermit<'a>(&'a Signal);
struct RequestMemory<'a> {
    signal: &'a Signal,
    bytes: usize,
}
impl Drop for RequestMemory<'_> {
    fn drop(&mut self) {
        let mut used = self.signal.request_bytes.lock().unwrap();
        *used -= self.bytes;
    }
}
impl Drop for HeavyPermit<'_> {
    fn drop(&mut self) {
        let mut count = self.0.heavy.lock().unwrap();
        *count -= 1;
        self.0.permit.notify_one();
    }
}
pub struct Client {
    endpoint: Arc<Mutex<Endpoint>>,
    config: RuntimeConfig,
    reconnect: Mutex<()>,
    lease: String,
    stop: Arc<AtomicBool>,
}
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn config_digest(c: &RuntimeConfig) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&c.model)?)
    ))
}
fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut size = [0; 4];
    stream.read_exact(&mut size)?;
    let n = u32::from_be_bytes(size) as usize;
    if n > 4 * 1024 * 1024 {
        return Err("IPC request too large".into());
    }
    let mut bytes = vec![0; n];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn write_frame(stream: &mut TcpStream, bytes: &[u8]) -> Result<()> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("IPC response too large".into());
    }
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)?;
    Ok(())
}
impl ProcessRuntime {
    pub fn attach(config: &RuntimeConfig) -> Result<Client> {
        fs::create_dir_all(&config.state_dir)?;
        private_state_directory(&config.state_dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(config.state_dir.join("launch.lock"))?;
        lock.lock_exclusive()?;
        let endpoint_file = config.state_dir.join("endpoint.json");
        let wanted = config_digest(config)?;
        if let Ok(bytes) = fs::read(&endpoint_file) {
            if let Ok(endpoint) = serde_json::from_slice::<Endpoint>(&bytes) {
                if endpoint.protocol_version != PROTOCOL_VERSION {
                    if let Ok(incumbent) = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(config.state_dir.join("daemon.lock"))
                    {
                        if incumbent.try_lock_exclusive().is_err() {
                            return Err("existing Agentlaw broker uses an incompatible private protocol; stop that broker and reconnect (no foreign process was terminated)".into());
                        }
                    }
                }
                let client = Client::new(endpoint, config.clone());
                if matches!(
                    rpc_to(
                        &client.endpoint.lock().unwrap(),
                        &client.lease,
                        Operation::Attach
                    ),
                    Ok(Response::State(_))
                ) {
                    if client.endpoint.lock().unwrap().config != wanted {
                        return Err("existing worker has different model configuration".into());
                    }
                    client.start_heartbeat();
                    return Ok(client);
                }
            }
        }
        let mut cmd = Command::new(&config.executable);
        cmd.arg("worker-daemon")
            .arg("--state-dir")
            .arg(&config.state_dir);
        if let Some(model) = &config.model {
            cmd.arg("--model")
                .arg(&model.onnx_model)
                .arg("--tokenizer")
                .arg(&model.tokenizer_json)
                .arg("--ort-library")
                .arg(&model.runtime_library);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000 | 0x00000200);
        }
        let mut child = cmd.spawn()?;
        for _ in 0..100 {
            if let Ok(bytes) = fs::read(&endpoint_file) {
                if let Ok(endpoint) = serde_json::from_slice::<Endpoint>(&bytes) {
                    if endpoint.config == wanted {
                        let client = Client::new(endpoint, config.clone());
                        if matches!(
                            rpc_to(
                                &client.endpoint.lock().unwrap(),
                                &client.lease,
                                Operation::Attach
                            ),
                            Ok(Response::State(_))
                        ) {
                            client.start_heartbeat();
                            return Ok(client);
                        }
                    }
                }
            }
            if child.try_wait()?.is_some() {
                return Err("worker daemon exited during startup".into());
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err("worker daemon endpoint not ready; startup status unknown".into())
    }
}
impl Client {
    pub fn bootstrap_status(&self, context: &DerivedContext) -> Result<(u64, bool)> {
        match self.rpc(Operation::BootstrapStatus {
            context: context.clone(),
        })? {
            Response::BootstrapStatus {
                next_page,
                complete,
            } => Ok((next_page, complete)),
            Response::Error(e) => Err(e.into()),
            _ => Err("invalid bootstrap status".into()),
        }
    }
    pub fn bootstrap_spooled(
        &self,
        context: &DerivedContext,
        page_number: u64,
        final_page: bool,
        page: &crate::spool::PublishedSpoolPage,
    ) -> Result<SourcePosition> {
        match self.rpc(Operation::BootstrapSpool {
            context: context.clone(),
            page_number,
            final_page,
            page: page.clone(),
        })? {
            Response::Position(p) => Ok(p),
            Response::Error(e) => Err(e.into()),
            _ => Err("invalid bootstrap response".into()),
        }
    }
    pub fn repair_index(&self, context: &DerivedContext) -> Result<crate::indexing::RepairReceipt> {
        match self.rpc(Operation::RepairIndex {
            context: context.clone(),
        })? {
            Response::RepairIndex(r) => Ok(r),
            Response::Error(e) => Err(e.into()),
            _ => Err("invalid index repair response".into()),
        }
    }
    pub fn stage_source_body(&self, reader: &mut impl Read) -> Result<crate::spool::SpoolBody> {
        Ok(crate::spool::stage_body(&self.config.state_dir, reader)?)
    }
    pub fn ingest_spooled(
        &self,
        context: &DerivedContext,
        page: &crate::spool::PublishedSpoolPage,
    ) -> Result<SourcePosition> {
        match self.rpc(Operation::DerivedSpool {
            context: context.clone(),
            page: page.clone(),
        })? {
            Response::Position(p) => Ok(p),
            Response::Error(e) => Err(e.into()),
            _ => Err("invalid spool ingest response".into()),
        }
    }
    pub fn execution_diagnostics(
        &self,
    ) -> Result<(Option<u64>, Option<crate::execution::Selection>)> {
        match self.rpc(Operation::ExecutionDiagnostics)? {
            Response::ExecutionDiagnostics {
                host_resident_bytes,
                selection,
                ..
            } => Ok((host_resident_bytes, selection)),
            Response::Error(e) => Err(e.into()),
            _ => Err("invalid execution diagnostics response".into()),
        }
    }
    pub fn resource_diagnostics(&self) -> Result<RuntimeResources> {
        match self.rpc(Operation::ExecutionDiagnostics)? {
            Response::ExecutionDiagnostics { resources, .. } => Ok(resources),
            Response::Error(e) => Err(e.into()),
            _ => Err("invalid resource diagnostics response".into()),
        }
    }
    pub fn ready_index_batch(
        &self,
        context: &DerivedContext,
        channel: crate::indexing::Channel,
        limit: usize,
    ) -> Result<Option<crate::indexing::IndexBatch>> {
        match self.rpc(Operation::IndexBatch {
            context: context.clone(),
            channel,
            limit,
        })? {
            Response::IndexBatch(v) => Ok(v),
            Response::Error(e) => Err(e.into()),
            _ => Err("invalid index batch response".into()),
        }
    }
    pub fn acknowledge_index(
        &self,
        context: &DerivedContext,
        receipt: &crate::indexing::IndexReceipt,
    ) -> Result<()> {
        match self.rpc(Operation::IndexAck {
            context: context.clone(),
            receipt: receipt.clone(),
        })? {
            Response::Ok => Ok(()),
            Response::Error(e) => Err(e.into()),
            _ => Err("invalid index ack response".into()),
        }
    }
    pub fn search_index(
        &self,
        context: &DerivedContext,
        query: &str,
        scopes: &[String],
        limit: usize,
        query_vector: Option<&[f32]>,
        required: SourcePosition,
    ) -> Result<SearchPacket> {
        self.search_index_cancellable(
            context,
            query,
            scopes,
            limit,
            query_vector,
            required,
            &AtomicBool::new(false),
        )
    }
    pub fn search_index_cancellable(
        &self,
        context: &DerivedContext,
        query: &str,
        scopes: &[String],
        limit: usize,
        query_vector: Option<&[f32]>,
        required: SourcePosition,
        cancel: &AtomicBool,
    ) -> Result<SearchPacket> {
        let operation = Operation::Search {
            context: context.clone(),
            query: query.into(),
            scopes: scopes.to_vec(),
            limit,
            vector: query_vector.map(|v| v.to_vec()),
            required,
        };
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(EmbeddingError::Cancelled.into());
            }
            match self.rpc_as_cancellable(&self.lease, operation.clone(), cancel)? {
                Response::Search(v) => return Ok(v),
                Response::Pending => {}
                Response::Error(e) => return Err(e.into()),
                _ => return Err("invalid index search response".into()),
            }
        }
    }
    fn new(endpoint: Endpoint, config: RuntimeConfig) -> Self {
        let lease = uuid::Uuid::new_v4().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        Self {
            endpoint: Arc::new(Mutex::new(endpoint)),
            config,
            reconnect: Mutex::new(()),
            lease,
            stop,
        }
    }
    fn start_heartbeat(&self) {
        let thread_stop = self.stop.clone();
        let e = self.endpoint.clone();
        let l = self.lease.clone();
        thread::spawn(move || {
            let mut ticks = 0;
            while !thread_stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(1));
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                ticks += 1;
                if ticks == 10 {
                    let endpoint = e.lock().unwrap().clone();
                    let _ = rpc_to(&endpoint, &l, Operation::Heartbeat);
                    ticks = 0;
                }
            }
        });
    }
    fn rpc(&self, operation: Operation) -> Result<Response> {
        self.rpc_as(&self.lease, operation)
    }
    fn rpc_as(&self, lease: &str, operation: Operation) -> Result<Response> {
        let endpoint = self.endpoint.lock().unwrap().clone();
        if let Ok(response) = rpc_to(&endpoint, lease, operation.clone()) {
            return Ok(response);
        }
        let _gate = self.reconnect.lock().unwrap();
        let current = self.endpoint.lock().unwrap().clone();
        if current.incarnation == endpoint.incarnation {
            let replacement = ProcessRuntime::attach(&self.config)?;
            let endpoint = replacement.endpoint.lock().unwrap().clone();
            match rpc_to(&endpoint, &self.lease, Operation::Attach)? {
                Response::State(_) => {}
                Response::Error(e) => return Err(e.into()),
                _ => return Err("reconnect attach failed".into()),
            }
            *self.endpoint.lock().unwrap() = endpoint;
        }
        rpc_to(&self.endpoint.lock().unwrap().clone(), lease, operation)
    }
    fn rpc_as_cancellable(
        &self,
        lease: &str,
        operation: Operation,
        cancel: &AtomicBool,
    ) -> Result<Response> {
        if cancel.load(Ordering::Relaxed) {
            return Err(EmbeddingError::Cancelled.into());
        }
        let endpoint = self.endpoint.lock().unwrap().clone();
        match rpc_to_cancellable(&endpoint, lease, operation.clone(), Some(cancel)) {
            Ok(v) => Ok(v),
            Err(_) if cancel.load(Ordering::Relaxed) => Err(EmbeddingError::Cancelled.into()),
            Err(_) => self.rpc_as(lease, operation),
        }
    }
    pub fn availability(&self) -> Result<SemanticAvailability> {
        match self.rpc(Operation::Heartbeat)? {
            Response::State(s) => Ok(s),
            _ => Err("invalid worker response".into()),
        }
    }
    pub fn derived_position(&self, context: &DerivedContext) -> Result<SourcePosition> {
        match self.rpc(Operation::DerivedPosition {
            context: context.clone(),
        })? {
            Response::Position(position) => Ok(position),
            Response::Error(error) => Err(error.into()),
            _ => Err("invalid derived cursor response".into()),
        }
    }
    pub fn ingest_published(
        &self,
        context: &DerivedContext,
        page: &PublishedPage,
    ) -> Result<SourcePosition> {
        match self.rpc(Operation::DerivedSubmit {
            context: context.clone(),
            page: page.clone(),
        })? {
            Response::Position(position) => Ok(position),
            Response::Error(error) => Err(error.into()),
            _ => Err("invalid derived admission response".into()),
        }
    }
    pub fn model_digest(&self) -> Result<String> {
        match self.rpc(Operation::Model)? {
            Response::Model(s) => Ok(s),
            Response::Error(e) => Err(e.into()),
            _ => Err("invalid worker model response".into()),
        }
    }
    pub fn embed(&self, input: &str) -> std::result::Result<Vec<f32>, EmbeddingError> {
        self.embed_cancellable(input, &AtomicBool::new(false))
    }
    pub fn embed_cancellable(
        &self,
        input: &str,
        cancel: &AtomicBool,
    ) -> std::result::Result<Vec<f32>, EmbeddingError> {
        let io =
            |e: Box<dyn std::error::Error + Send + Sync>| EmbeddingError::Failed(e.to_string());
        let operation_lease = OperationLease {
            client: self,
            id: format!("op:{}", uuid::Uuid::new_v4()),
        };
        let call = |operation| self.rpc_as_cancellable(&operation_lease.id, operation, cancel);
        let id = match call(Operation::Submit {
            input: input.into(),
        })
        .map_err(io)?
        {
            Response::Job(id) => id,
            Response::Error(e) => return Err(e),
            _ => return Err(EmbeddingError::Failed("invalid submit response".into())),
        };
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = call(Operation::Cancel { id });
                return Err(EmbeddingError::Cancelled);
            }
            let response = call(Operation::Poll { id }).map_err(|e| {
                if cancel.load(Ordering::Relaxed) {
                    EmbeddingError::Cancelled
                } else {
                    io(e)
                }
            })?;
            match response {
                Response::Vector(v) => return Ok(v),
                Response::Error(e) => return Err(e),
                Response::Pending => {}
                _ => return Err(EmbeddingError::Failed("invalid poll response".into())),
            }
        }
    }
}
struct OperationLease<'a> {
    client: &'a Client,
    id: String,
}
impl Drop for OperationLease<'_> {
    fn drop(&mut self) {
        let endpoint = self.client.endpoint.lock().unwrap().clone();
        let _ = rpc_to(&endpoint, &self.id, Operation::Detach);
    }
}
fn rpc_to(endpoint: &Endpoint, lease: &str, operation: Operation) -> Result<Response> {
    rpc_to_cancellable(endpoint, lease, operation, None)
}
fn rpc_to_cancellable(
    endpoint: &Endpoint,
    lease: &str,
    operation: Operation,
    cancel: Option<&AtomicBool>,
) -> Result<Response> {
    let mut socket = TcpStream::connect(&endpoint.address)?;
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(35)))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))?;
    write_frame(
        &mut socket,
        &serde_json::to_vec(&Request {
            protocol_version: PROTOCOL_VERSION,
            incarnation: endpoint.incarnation.clone(),
            secret: endpoint.secret.clone(),
            lease: lease.into(),
            operation,
        })?,
    )?;
    if let Some(cancel) = cancel {
        socket.set_read_timeout(Some(Duration::from_millis(50)))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(35);
        let mut read = |buffer: &mut [u8]| -> Result<()> {
            let mut offset = 0;
            while offset < buffer.len() {
                if cancel.load(Ordering::Relaxed) {
                    return Err(EmbeddingError::Cancelled.into());
                }
                if std::time::Instant::now() >= deadline {
                    return Err("worker response deadline elapsed".into());
                }
                match socket.read(&mut buffer[offset..]) {
                    Ok(0) => return Err("worker closed response stream".into()),
                    Ok(n) => offset += n,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                        ) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(())
        };
        let mut size = [0u8; 4];
        read(&mut size)?;
        let n = u32::from_be_bytes(size) as usize;
        if n > 4 * 1024 * 1024 {
            return Err("IPC response too large".into());
        }
        let mut bytes = vec![0; n];
        read(&mut bytes)?;
        Ok(serde_json::from_slice(&bytes)?)
    } else {
        Ok(serde_json::from_slice(&read_frame(&mut socket)?)?)
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let endpoint = self.endpoint.lock().unwrap().clone();
        let _ = rpc_to(&endpoint, &self.lease, Operation::Detach);
    }
}
/// Hidden CLI route: `worker-daemon --state-dir PATH [--model FILE --tokenizer FILE --ort-library DLL]`.
pub fn run_daemon(config: RuntimeConfig) -> Result<()> {
    fs::create_dir_all(&config.state_dir)?;
    private_state_directory(&config.state_dir)?;
    let singleton = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(config.state_dir.join("daemon.lock"))?;
    if singleton.try_lock_exclusive().is_err() {
        return Ok(());
    }
    let db = config.state_dir.join("broker.sqlite");
    let mut broker = Broker::open(&db, 64)?;
    DerivedWorkCoordinator::new(&mut broker, &MaterializedSource(None))?;
    broker.connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS diagnostics(name TEXT PRIMARY KEY,cause TEXT NOT NULL);CREATE TABLE IF NOT EXISTS execution_selection(fingerprint TEXT PRIMARY KEY,report TEXT NOT NULL);CREATE TABLE IF NOT EXISTS worker_recovery(config TEXT PRIMARY KEY,streak INTEGER NOT NULL,next_at INTEGER NOT NULL,blocked TEXT);",
    )?;
    broker
        .connection
        .execute("DELETE FROM diagnostics WHERE name='supervisor_fatal'", [])?;
    // Holding the OS lock proves the previous daemon is dead, so invalidate late results.
    broker
        .connection
        .execute("UPDATE worker SET state='failed'", [])?;
    let incarnation = uuid::Uuid::new_v4().to_string(); // broker endpoint, distinct from model incarnation
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = Endpoint {
        protocol_version: PROTOCOL_VERSION,
        incarnation: incarnation.clone(),
        address: listener.local_addr()?.to_string(),
        secret: format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
        config: config_digest(&config)?,
    };
    let path = config.state_dir.join("endpoint.json");
    let temporary = config
        .state_dir
        .join(format!("endpoint-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&serde_json::to_vec(&endpoint)?)?;
    file.sync_all()?;
    drop(file);
    // An old endpoint belongs to a dead incarnation, proven by daemon.lock.
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(&temporary, &path)?;
    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::new(Signal::default());
    let worker_signal = signal.clone();
    let worker_stop = stop.clone();
    let worker_config = config.clone();
    let mut computation = Some(thread::spawn(move || {
        crate::supervisor::run(worker_config, worker_stop, worker_signal)
    }));
    let index_signal = signal.clone();
    let index_stop = stop.clone();
    let index_state = config.state_dir.clone();
    let indexer = thread::spawn(move || -> Result<()> {
        let mut b = Broker::open(index_state.join("broker.sqlite"), 64)?;
        while !index_stop.load(Ordering::Relaxed) {
            let observed = index_signal.version();
            index_background_pass(&mut b, &index_state)?;
            index_signal.wait(observed, Duration::from_secs(30));
        }
        Ok(())
    });
    let (incoming_tx, incoming_rx) = std::sync::mpsc::sync_channel(64);
    thread::spawn(move || {
        for accepted in listener.incoming() {
            if incoming_tx.send(accepted).is_err() {
                break;
            }
        }
    });
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut last_heartbeat = 0;
    while !stop.load(Ordering::Relaxed) {
        if now() - last_heartbeat >= 1000 {
            if computation
                .as_ref()
                .is_some_and(|handle| handle.is_finished())
            {
                let outcome = computation.take().unwrap().join();
                let cause = match outcome {
                    Ok(Err(error)) => Some(error.to_string()),
                    Err(_) => Some("model inference thread panicked".into()),
                    Ok(Ok(())) => None,
                };
                if let Some(cause) = cause {
                    let tx = broker.connection.transaction()?;
                    tx.execute("INSERT INTO diagnostics VALUES('supervisor_fatal',?1) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",[&cause])?;
                    tx.execute("INSERT INTO diagnostics VALUES('model_load',?1) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",[&cause])?;
                    tx.execute("UPDATE worker SET state='failed'", [])?;
                    tx.execute(
                        "UPDATE jobs SET state='failed',error=?1 WHERE state='running'",
                        [cause],
                    )?;
                    tx.commit()?;
                }
            }
            broker
                .connection
                .execute("UPDATE worker SET heartbeat=?1", [now()])?;
            last_heartbeat = now();
            // Disposable query cache only. Never removes durable derived work or source.
            broker.connection.execute("DELETE FROM jobs WHERE durable=0 AND state!='running' AND id NOT IN(SELECT job FROM waiters) AND id NOT IN(SELECT id FROM jobs WHERE durable=0 ORDER BY id DESC LIMIT 1024)",[])?;
            let _ = broker.idle_shutdown_due(now(), 600000)?; // Lease cleanup; supervisor owns model idle stop.
        }
        match incoming_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(mut socket)) => {
                if active.load(Ordering::Relaxed) >= 64 {
                    continue;
                }
                socket.set_nodelay(true)?;
                socket.set_read_timeout(Some(Duration::from_secs(2)))?;
                socket.set_write_timeout(Some(Duration::from_secs(2)))?;
                let db = db.clone();
                let endpoint = endpoint.clone();
                let signal = signal.clone();
                let state = config.state_dir.clone();
                let active = active.clone();
                active.fetch_add(1, Ordering::Relaxed);
                thread::spawn(move || {
                    let result = (|| -> Result<()> {
                        let mut broker = Broker::open(db, 64)?;
                        let response =
                            handle_request(&mut broker, &endpoint, &mut socket, &signal, &state)
                                .unwrap_or_else(|e| {
                                    Response::Error(EmbeddingError::Failed(e.to_string()))
                                });
                        let _ = write_frame(&mut socket, &serde_json::to_vec(&response)?);
                        Ok(())
                    })();
                    let _ = result;
                    active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Ok(Err(e)) => return Err(e.into()),
            Err(e) => return Err(e.into()),
        }
    }
    stop.store(true, Ordering::Relaxed);
    signal.notify();
    if let Some(computation) = computation {
        let _ = computation.join();
    }
    let _ = indexer.join();
    broker.connection.execute(
        "UPDATE worker SET state='stopped' WHERE incarnation=?1",
        [&incarnation],
    )?;
    let _ = fs::remove_file(path);
    Ok(())
}
fn handle_request(
    broker: &mut Broker,
    endpoint: &Endpoint,
    socket: &mut TcpStream,
    signal: &Signal,
    state: &std::path::Path,
) -> Result<Response> {
    let mut header = [0u8; 4];
    socket.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header) as usize;
    if size > 4 * 1024 * 1024 {
        return Err("IPC request too large".into());
    }
    let _memory = signal.reserve(size.checked_mul(2).ok_or("request memory overflow")?)?;
    let mut payload = vec![0u8; size];
    socket.read_exact(&mut payload)?;
    let req: Request = serde_json::from_slice(&payload)?;
    drop(payload);
    if req.protocol_version != PROTOCOL_VERSION || req.incarnation != endpoint.incarnation {
        return Err("worker protocol/incarnation mismatch".into());
    }
    if req.secret.len() != endpoint.secret.len()
        || req
            .secret
            .bytes()
            .zip(endpoint.secret.bytes())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            != 0
    {
        return Err("authentication failed".into());
    }
    let _heavy = if matches!(
        &req.operation,
        Operation::Search { .. }
            | Operation::BootstrapSpool { .. }
            | Operation::DerivedSpool { .. }
            | Operation::DerivedSubmit { .. }
            | Operation::RepairIndex { .. }
    ) {
        Some(signal.acquire())
    } else {
        None
    };
    match req.operation {
        Operation::BootstrapStatus { context } => {
            let source = MaterializedSource(None);
            let _ = DerivedWorkCoordinator::new(broker, &source)?;
            let status:Option<(u64,bool)>=broker.connection.query_row("SELECT next_page,complete FROM derived_bootstrap WHERE repository=?1 AND model=?2 AND config=?3",rusqlite::params![context.repository_id,context.model_digest,context.config_digest],|r|Ok((r.get::<_,i64>(0)? as u64,r.get(1)?))).optional()?;
            let (next_page, complete) = status.unwrap_or((0, false));
            Ok(Response::BootstrapStatus {
                next_page,
                complete,
            })
        }
        Operation::BootstrapSpool {
            context,
            page_number,
            final_page,
            page,
        } => {
            let _ = DerivedWorkCoordinator::new(broker, &MaterializedSource(None))?;
            if crate::derived::bootstrap_receipt(
                broker,
                &context,
                page_number,
                final_page,
                &page.page,
                &page.bodies,
            )? {
                signal.notify();
                return Ok(Response::Position(context.initial_basis.clone()));
            }
            let source = MaterializedSource(Some(page.page));
            let position = DerivedWorkCoordinator::new(broker, &source)?.bootstrap_with_spools(
                &context,
                page_number,
                final_page,
                state,
                &page.bodies,
            )?;
            for reference in &page.bodies {
                let _ = crate::spool::release_body(state, &reference.body);
            }
            signal.notify();
            while crate::indexing::flush_channel(
                broker,
                &context,
                state,
                crate::indexing::Channel::Lexical,
            )? {}
            signal.notify();
            Ok(Response::Position(position))
        }
        Operation::RepairIndex { context } => {
            let receipt = crate::indexing::repair_index(broker, &context, state)?;
            signal.notify();
            Ok(Response::RepairIndex(receipt))
        }
        Operation::DerivedSpool { context, page } => {
            if let Some(position) =
                crate::derived::accepted_receipt(broker, &context, &page.page, &page.bodies)?
            {
                signal.notify();
                return Ok(Response::Position(position));
            }
            let limit = (page.page.batches.len() as u32).max(1);
            let source = MaterializedSource(Some(page.page));
            let mut coordinator = DerivedWorkCoordinator::new(broker, &source)?;
            let position =
                coordinator.advance_with_spools(&context, limit, Some((state, &page.bodies)))?;
            for reference in &page.bodies {
                if let Err(error) = crate::spool::release_body(state, &reference.body) {
                    broker.connection.execute("INSERT INTO diagnostics VALUES('spool_cleanup',?1) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",[error.to_string()])?;
                }
            }
            signal.notify();
            while crate::indexing::flush_channel(
                broker,
                &context,
                state,
                crate::indexing::Channel::Lexical,
            )? {}
            signal.notify();
            Ok(Response::Position(position))
        }
        Operation::ExecutionDiagnostics => {
            let json = broker
                .connection
                .query_row(
                    "SELECT report FROM execution_selection ORDER BY rowid DESC LIMIT 1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            let broker_resident_bytes = crate::execution::host_resident_bytes();
            let resource: Option<String> = broker
                .connection
                .query_row(
                    "SELECT cause FROM diagnostics WHERE name='worker_resources'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let resource =
                resource.and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok());
            let (inc, state): (String, String) =
                broker
                    .connection
                    .query_row("SELECT incarnation,state FROM worker", [], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?;
            let model_resident_bytes = resource
                .as_ref()
                .filter(|v| {
                    v["incarnation"].as_str() == Some(&inc)
                        && matches!(state.as_str(), "ready" | "loading")
                        && v["observed_ms"]
                            .as_i64()
                            .is_some_and(|t| (0..=30000).contains(&now().saturating_sub(t)))
                })
                .and_then(|v| v["model_resident_bytes"].as_u64());
            let resources = RuntimeResources {
                broker_resident_bytes,
                model_resident_bytes,
                combined_resident_bytes: broker_resident_bytes
                    .zip(model_resident_bytes)
                    .and_then(|(a, b)| a.checked_add(b)),
            };
            Ok(Response::ExecutionDiagnostics {
                host_resident_bytes: resources.combined_resident_bytes,
                selection: json.map(|v| serde_json::from_str(&v)).transpose()?,
                resources,
            })
        }
        Operation::DerivedPosition { context } => {
            let source = MaterializedSource(None);
            let coordinator = DerivedWorkCoordinator::new(broker, &source)?;
            Ok(Response::Position(coordinator.position(&context)?))
        }
        Operation::DerivedSubmit { context, page } => {
            if let Some(position) = crate::derived::accepted_receipt(broker, &context, &page, &[])?
            {
                signal.notify();
                return Ok(Response::Position(position));
            }
            let limit = (page.batches.len() as u32).max(1);
            let source = MaterializedSource(Some(page));
            let mut coordinator = DerivedWorkCoordinator::new(broker, &source)?;
            let position = coordinator.advance(&context, limit)?;
            signal.notify();
            while crate::indexing::flush_channel(
                broker,
                &context,
                state,
                crate::indexing::Channel::Lexical,
            )? {}
            signal.notify();
            Ok(Response::Position(position))
        }
        Operation::IndexBatch {
            context,
            channel,
            limit,
        } => Ok(Response::IndexBatch(crate::indexing::ready_index_batch(
            broker, &context, channel, limit,
        )?)),
        Operation::IndexAck { context, receipt } => {
            crate::indexing::verify_receipt(state, &context, &receipt)?;
            crate::indexing::acknowledge_index(broker, &context, &receipt)?;
            signal.notify();
            Ok(Response::Ok)
        }
        Operation::Search {
            context,
            query,
            scopes,
            limit,
            vector,
            required,
        } => search(
            broker,
            state,
            signal,
            &context,
            &query,
            &scopes,
            limit,
            vector.as_deref(),
            required,
        ),
        Operation::Attach | Operation::Heartbeat => {
            broker.renew_lease(&req.lease, false, now(), 60000)?;
            signal.notify();
            let mut availability = broker.availability()?;
            if matches!(
                availability,
                SemanticAvailability::Failed | SemanticAvailability::Unavailable
            ) {
                let failed:bool=broker.connection.query_row("SELECT EXISTS(SELECT 1 FROM worker_recovery WHERE config=?1 AND (streak>0 OR blocked IS NOT NULL)) OR EXISTS(SELECT 1 FROM diagnostics WHERE name='supervisor_fatal')",[&endpoint.config],|r|r.get(0))?;
                if !failed {
                    availability = SemanticAvailability::Loading;
                }
            }
            Ok(Response::State(availability))
        }
        Operation::Model => {
            if broker.availability()? != SemanticAvailability::Ready {
                return Ok(Response::Error(EmbeddingError::Unavailable(
                    "model not ready".into(),
                )));
            }
            Ok(Response::Model(broker.connection.query_row(
                "SELECT model FROM worker",
                [],
                |r| r.get(0),
            )?))
        }
        Operation::Detach => {
            broker.release_lease(&req.lease)?;
            signal.notify();
            Ok(Response::Ok)
        }
        Operation::Submit { input } => {
            match broker.availability()? {
                SemanticAvailability::Ready => {}
                SemanticAvailability::Loading => {
                    return Ok(Response::Error(EmbeddingError::Loading))
                }
                SemanticAvailability::Failed => {
                    let cause: String = broker
                        .connection
                        .query_row(
                            "SELECT cause FROM diagnostics WHERE name='model_load'",
                            [],
                            |r| r.get(0),
                        )
                        .unwrap_or_else(|_| "model worker failed".into());
                    return Ok(Response::Error(EmbeddingError::Unavailable(cause)));
                }
                SemanticAvailability::Unavailable => {
                    return Ok(Response::Error(EmbeddingError::Unavailable(
                        "worker stopped".into(),
                    )))
                }
            }
            broker.renew_lease(&req.lease, true, now(), 60000)?;
            let digest = format!("{:x}", Sha256::digest(input.as_bytes()));
            let model_digest =
                broker
                    .connection
                    .query_row("SELECT model FROM worker", [], |r| r.get(0))?;
            let key = JobKey {
                model_digest,
                config_digest: endpoint.config.clone(),
                memory_id: "query".into(),
                change_id: digest.clone(),
                content_digest: digest,
                section: "query".into(),
            };
            let id = match broker.enqueue(&key, &input, false, true, Some(&req.lease))? {
                Admission::Accepted(id) | Admission::Joined(id) => id,
            };
            signal.notify();
            Ok(Response::Job(id))
        }
        Operation::Poll { id } => {
            let deadline = std::time::Instant::now() + Duration::from_secs(25);
            loop {
                let observed = signal.version();
                broker.renew_lease(&req.lease, true, now(), 60000)?;
                let failed: Option<String> = broker
                    .connection
                    .query_row(
                        "SELECT cause FROM waiter_failures WHERE job=?1 AND lease=?2",
                        rusqlite::params![id, req.lease],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(cause) = failed {
                    return Ok(Response::Error(EmbeddingError::Failed(cause)));
                }
                let owned: i64 = broker.connection.query_row(
                    "SELECT COUNT(*) FROM waiters WHERE job=?1 AND lease=?2",
                    rusqlite::params![id, req.lease],
                    |r| r.get(0),
                )?;
                if owned == 0 {
                    return Err("job not attached to client".into());
                }
                let (state, error): (String, Option<String>) = broker.connection.query_row(
                    "SELECT state,error FROM jobs WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if state == "failed" {
                    return Ok(Response::Error(EmbeddingError::Failed(
                        error.unwrap_or_default(),
                    )));
                } else if let Some(v) = broker.job_result(id)? {
                    return Ok(Response::Vector(v));
                } else if broker.availability()? == SemanticAvailability::Failed {
                    return Ok(Response::Error(EmbeddingError::Failed(
                        "worker failed before pending inference completed".into(),
                    )));
                } else {
                    let Some(wait) = deadline.checked_duration_since(std::time::Instant::now())
                    else {
                        return Ok(Response::Pending);
                    };
                    signal.wait(observed, wait);
                }
            }
        }
        Operation::Cancel { id } => {
            broker.connection.execute(
                "DELETE FROM waiter_failures WHERE job=?1 AND lease=?2",
                rusqlite::params![id, req.lease],
            )?;
            broker.connection.execute(
                "DELETE FROM waiters WHERE job=?1 AND lease=?2",
                rusqlite::params![id, req.lease],
            )?;
            Ok(Response::Ok)
        }
    }
}
struct MaterializedSource(Option<PublishedPage>);
pub(crate) fn index_background_pass(b: &mut Broker, state: &std::path::Path) -> Result<()> {
    if let Err(error) = flush_vectors(b, state) {
        b.connection.execute("INSERT INTO diagnostics VALUES('index_background',?1) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",[error.to_string()])?;
        b.connection.execute("INSERT INTO diagnostics VALUES('index_background_status','active') ON CONFLICT(name) DO UPDATE SET cause='active'",[])?;
    } else {
        b.connection.execute("UPDATE diagnostics SET cause='resolved; last failure retained in index_background' WHERE name='index_background_status'",[])?;
    }
    Ok(())
}
pub(crate) fn flush_vectors(b: &mut Broker, state: &std::path::Path) -> Result<()> {
    let stored_contexts = {
        let mut st = b.connection.prepare("SELECT json FROM derived_contexts")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let contexts: Vec<DerivedContext> = stored_contexts
        .iter()
        .map(|json| serde_json::from_str(json))
        .collect::<std::result::Result<_, _>>()?;
    let mut failed_repositories = std::collections::HashSet::new();
    let mut first_error: Option<String> = None;
    for c in &contexts {
        loop {
            match crate::indexing::flush_channel(b, c, state, crate::indexing::Channel::Vector) {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    let detail = format!(
                        "model={} config={}: {error}",
                        c.model_digest, c.config_digest
                    );
                    b.connection.execute("INSERT INTO diagnostics VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",rusqlite::params![format!("vector_index:{}",c.repository_id),detail])?;
                    failed_repositories.insert(c.repository_id.clone());
                    first_error.get_or_insert_with(|| error.to_string());
                    break;
                }
            }
        }
    }
    let repositories: std::collections::HashSet<&str> =
        contexts.iter().map(|c| c.repository_id.as_str()).collect();
    for repository in repositories {
        if failed_repositories.contains(repository) {
            continue;
        }
        let name = format!("vector_index:{repository}");
        let active: bool = b.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM diagnostics WHERE name=?1)",
            [&name],
            |r| r.get(0),
        )?;
        if !active {
            continue;
        }
        let mut verified = true;
        for c in contexts.iter().filter(|c| c.repository_id == repository) {
            match crate::indexing::verify_vector_recovery(b, c, state) {
                Ok(true) => {}
                Ok(false) => verified = false,
                Err(error) => {
                    let detail = format!(
                        "model={} config={}: {error}",
                        c.model_digest, c.config_digest
                    );
                    b.connection.execute("INSERT INTO diagnostics VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",rusqlite::params![name,detail])?;
                    first_error.get_or_insert_with(|| error.to_string());
                    verified = false;
                    break;
                }
            }
        }
        if verified {
            crate::supervisor::resolve_diagnostic(b, &name, None)?;
        }
    }
    if let Some(error) = first_error {
        return Err(error.into());
    }
    let unresolved: Option<String> = b
        .connection
        .query_row(
            "SELECT name FROM diagnostics WHERE name LIKE 'vector_index:%' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(name) = unresolved {
        return Err(format!("{name}: recovery not yet verified").into());
    }
    Ok(())
}
fn search(
    b: &mut Broker,
    state: &std::path::Path,
    signal: &Signal,
    c: &DerivedContext,
    query: &str,
    scopes: &[String],
    limit: usize,
    vector: Option<&[f32]>,
    required: SourcePosition,
) -> Result<Response> {
    use crate::indexing::{channel_position, flush_channel, Channel};
    use agentlaw_search::{ExactVectorIndex, ScopeFilter, SearchIndex};
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    if required.epoch != c.initial_basis.epoch {
        return Err("source epoch mismatch".into());
    }
    loop {
        let observed = signal.version();
        while flush_channel(b, c, state, Channel::Lexical)? {}
        if vector.is_some() {
            while flush_channel(b, c, state, Channel::Vector)? {}
        }
        let lex = channel_position(b, c, Channel::Lexical)?;
        let vec = channel_position(b, c, Channel::Vector)?;
        if lex.sequence >= required.sequence
            && (vector.is_none() || vec.sequence >= required.sequence)
            && crate::indexing::bootstrap_channel_complete(b, c, Channel::Lexical)?
            && (vector.is_none()
                || crate::indexing::bootstrap_channel_complete(b, c, Channel::Vector)?)
        {
            break;
        }
        if vector.is_some() && b.availability()? != SemanticAvailability::Ready {
            return Ok(Response::Error(EmbeddingError::Unavailable(
                "model not ready for requested vector fence".into(),
            )));
        }
        let Some(wait) = deadline.checked_duration_since(std::time::Instant::now()) else {
            return Ok(Response::Pending);
        };
        signal.wait(observed, wait);
    }
    let generation_lease = crate::indexing::generation_lease(state, c)?;
    let dir = generation_lease
        .as_ref()
        .map(|g| g.directory.clone())
        .unwrap_or(crate::indexing::directory(state, c)?);
    let scope = ScopeFilter {
        allowed_scopes: scopes.to_vec(),
    };
    let mut index = SearchIndex::open(dir.join("lexical.sqlite"))?;
    index.prepare_scope(&scope)?;
    let mut view = index.read_view()?;
    if view.stamp.ack < required.sequence as i64 {
        return Err("lexical immutable view does not cover source fence; rebuild required".into());
    }
    let lexical = view.search(query, &scope, limit, &[], &[], true)?;
    let lexical_strength = view.lexical_strength(query, &scope, limit)?;
    let lexical_matched = view.lexical_matched(query, &scope)?;
    let (vector, vector_stamp, vector_candidates) = if let Some(query_vector) = vector {
        let view = ExactVectorIndex::open(
            dir.join("vector.sqlite"),
            &c.model_digest,
            &c.config_digest,
            256,
        )?
        .read_view()?;
        if view.stamp.ack < required.sequence as i64 {
            return Err(
                "vector immutable view does not cover source fence; rebuild required".into(),
            );
        }
        let (hits, metrics) = view.search_ann(query_vector, &scope, limit)?;
        (
            hits,
            Some(view.stamp.clone()),
            metrics.memory_candidates as u64,
        )
    } else {
        (Vec::new(), None, 0)
    };
    Ok(Response::Search(SearchPacket {
        lexical_matched,
        vector_candidates,
        lexical,
        lexical_strength,
        vector,
        lexical_stamp: view.stamp,
        semantic_complete: vector_stamp.is_some(),
        vector_stamp,
        source_position: required,
    }))
}
impl PublishedSourcePort for MaterializedSource {
    fn read_published_changes(
        &self,
        _: &str,
        basis: &SourcePosition,
        _: u32,
    ) -> crate::derived::Result<PublishedPage> {
        let page = self.0.clone().ok_or_else(|| {
            crate::derived::Error::SourceUnavailable("no published page supplied".into())
        })?;
        if &page.basis != basis {
            return Err(crate::derived::Error::CoverageLost(
                "source page basis does not match durable queue cursor".into(),
            ));
        }
        Ok(page)
    }
}
fn private_state_directory(path: &std::path::Path) -> Result<()> {
    let canonical = fs::canonicalize(path)?;
    if canonical.parent().is_none()
        || std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .and_then(|p| fs::canonicalize(p).ok())
            .as_ref()
            == Some(&canonical)
    {
        return Err("worker state must use a dedicated directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let script="$ErrorActionPreference='Stop'; $taskSid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; $taskAcl=New-Object System.Security.AccessControl.DirectorySecurity; $taskAcl.SetAccessRuleProtection($true,$false); $taskRule=New-Object System.Security.AccessControl.FileSystemAccessRule($taskSid,'FullControl','ContainerInherit,ObjectInherit','None','Allow'); $taskAcl.AddAccessRule($taskRule); [System.IO.Directory]::SetAccessControl($env:AGENTLAW_PRIVATE_STATE_PATH,$taskAcl)";
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("AGENTLAW_PRIVATE_STATE_PATH", &canonical)
            .creation_flags(0x08000000)
            .output()?;
        if !output.status.success() {
            return Err("unable to restrict worker state directory to current OS user".into());
        }
    }
    Ok(())
}
