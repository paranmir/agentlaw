//! One serialized owner for model-process transitions. Native inference is never in this process.
use crate::{
    model_child::{self, Input, Launch, Output, Result},
    process::{RuntimeConfig, Signal},
    Broker, EmbeddingError, Job, WorkerState, MODEL_ID,
};
use fs2::FileExt;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
enum Event {
    Output(String, Output),
    Exit(String),
    Pipe(String, String),
    ControlLost(String, String),
}
/// OS I/O is separated from transition policy; a test adapter can replace this boundary.
trait WorkerProcessPort {
    fn spawn(&self, config: &RuntimeConfig) -> Result<Child>;
    fn observe(
        &self,
        child: &Child,
        inc: String,
        events: mpsc::Sender<Event>,
        signal: Arc<Signal>,
    ) -> Result<()>;
    fn stop(&self, child: &mut Child) -> Result<()>;
    fn control(&self, address: &str, inc: &str, secret: &str, stop: bool) -> Result<Option<u64>>;
}
struct NativeProcess;
impl WorkerProcessPort for NativeProcess {
    fn spawn(&self, c: &RuntimeConfig) -> Result<Child> {
        let mut cmd = Command::new(&c.executable);
        cmd.arg("model-child")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        Ok(cmd.spawn()?)
    }
    fn observe(
        &self,
        child: &Child,
        inc: String,
        events: mpsc::Sender<Event>,
        signal: Arc<Signal>,
    ) -> Result<()> {
        observe_exit(child, move || {
            let _ = events.send(Event::Exit(inc));
            signal.notify();
        })
    }
    fn stop(&self, child: &mut Child) -> Result<()> {
        if child.try_wait()?.is_none() {
            child.kill()?;
        }
        child.wait()?;
        Ok(())
    }
    fn control(&self, address: &str, inc: &str, secret: &str, stop: bool) -> Result<Option<u64>> {
        model_child::control(address, inc, secret, stop)
    }
}
#[cfg(windows)]
fn observe_exit(child: &Child, notify: impl FnOnce() + Send + 'static) -> Result<()> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn DuplicateHandle(
            source: *mut c_void,
            handle: *mut c_void,
            target: *mut c_void,
            output: *mut *mut c_void,
            access: u32,
            inherit: i32,
            options: u32,
        ) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    let mut handle = std::ptr::null_mut();
    unsafe {
        let process = GetCurrentProcess();
        if DuplicateHandle(
            process,
            child.as_raw_handle(),
            process,
            &mut handle,
            0,
            0,
            2,
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let handle = handle as usize;
    std::thread::spawn(move || unsafe {
        let result = WaitForSingleObject(handle as *mut c_void, u32::MAX);
        CloseHandle(handle as *mut c_void);
        if result == 0 {
            notify();
        }
    });
    Ok(())
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn observe_exit(child: &Child, notify: impl FnOnce() + Send + 'static) -> Result<()> {
    // WNOWAIT observes exit without reaping/releasing the PID: the owning Child
    // remains the sole reaper and termination authority until the event is handled.
    let pid = libc::id_t::try_from(child.id())?;
    std::thread::spawn(move || {
        // libc supplies the platform ABI and flags: notably macOS WNOWAIT differs
        // from Linux. waitid initializes siginfo; no guessed size/alignment is used.
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::uninit();
        loop {
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if result == 0 {
                notify();
                break;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        }
    });
    Ok(())
}
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn observe_exit(_: &Child, _: impl FnOnce() + Send + 'static) -> Result<()> {
    Err("native child exit adapter is unavailable on this platform".into())
}
struct Active {
    child: OwnedChild,
    inc: String,
    secret: String,
    control: Option<String>,
    job: Option<Job>,
    ready_since: Option<Instant>,
    last_control: Instant,
    unavailable: bool,
    exited: bool,
}
struct OwnedChild(Child);
impl std::ops::Deref for OwnedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
enum Slot {
    Stopped,
    Active(Active),
    Waiting(Instant),
    Blocked(String),
}
struct Recovery {
    streak: i64,
    config: String,
}
fn now() -> i64 {
    crate::process::now()
}
fn diagnostic(b: &Broker, name: &str, cause: &str) -> Result<()> {
    b.connection.execute("INSERT INTO diagnostics VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",params![name,cause])?;
    Ok(())
}
fn heartbeat(port: &impl WorkerProcessPort, b: &Broker, active: &mut Active) -> Result<()> {
    let address = active.control.as_ref().ok_or("model control unavailable")?;
    let model = port.control(address, &active.inc, &active.secret, false)?;
    let broker = crate::execution::host_resident_bytes();
    let total = broker.zip(model).and_then(|(a, b)| a.checked_add(b));
    active.last_control = Instant::now();
    // A diagnostic disk error cannot negate an authenticated live-control proof.
    let _=diagnostic(b,"worker_resources",&serde_json::json!({"incarnation":active.inc,"observed_ms":now(),"broker_resident_bytes":broker,"model_resident_bytes":model,"combined_resident_bytes":total}).to_string());
    Ok(())
}
fn demand(b: &Broker, config: &str) -> Result<bool> {
    Ok(b.connection.query_row("SELECT EXISTS(SELECT 1 FROM leases WHERE expires>?1) OR EXISTS(SELECT 1 FROM jobs WHERE durable=1 AND model=(SELECT model FROM worker_model_configs WHERE config=?2) AND state IN('queued','running','retry_wait'))",params![now(),config],|r|r.get(0))?)
}
fn fingerprint(c: &RuntimeConfig) -> Result<String> {
    let mut h = Sha256::new();
    h.update(serde_json::to_vec(&c.model)?);
    if let Some(a) = &c.model {
        for p in [&a.onnx_model, &a.tokenizer_json, &a.runtime_library] {
            match std::fs::metadata(p) {
                Ok(m) => {
                    h.update(m.len().to_le_bytes());
                    h.update(format!("{:?}", m.modified()).as_bytes());
                }
                Err(e) => h.update(e.to_string().as_bytes()),
            }
        }
    }
    Ok(format!("{:x}", h.finalize()))
}
fn lose_claims(b: &mut Broker, inc: &str, cause: &str) -> Result<()> {
    // This transaction touches only C7 claims/waiters/control state, never source/index ACKs.
    let tx = b
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("INSERT OR IGNORE INTO waiter_failures(job,lease,cause) SELECT w.job,w.lease,?1 FROM waiters w JOIN jobs j ON j.id=w.job WHERE j.state IN('queued','running','retry_wait')",[cause])?;
    tx.execute("UPDATE jobs SET state='failed',error=?1,payload='' WHERE durable=0 AND state IN('queued','running','retry_wait')",[cause])?;
    tx.execute("UPDATE jobs SET state='retry_wait',next_attempt=?1,incarnation=NULL,error=?2 WHERE durable=1 AND state='running' AND incarnation=?3",params![now(),cause,inc])?;
    tx.execute(
        "UPDATE worker SET state='failed' WHERE incarnation=?1",
        [inc],
    )?;
    tx.commit()?;
    diagnostic(b, "worker_supervision", cause)
}
impl Recovery {
    fn load(b: &Broker, config: String) -> Result<(Self, Slot)> {
        let row: Option<(i64, i64, Option<String>)> = b
            .connection
            .query_row(
                "SELECT streak,next_at,blocked FROM worker_recovery WHERE config=?1",
                [&config],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (streak, next, blocked) = row.unwrap_or((0, 0, None));
        let slot = if let Some(f) = blocked {
            Slot::Blocked(f)
        } else {
            Slot::Waiting(
                Instant::now()
                    + Duration::from_millis(next.saturating_sub(now()).clamp(0, 300000) as u64),
            )
        };
        Ok((Self { streak, config }, slot))
    }
    fn save(&self, b: &Broker, next: i64, blocked: Option<&str>) -> Result<()> {
        b.connection.execute("INSERT INTO worker_recovery VALUES(?1,?2,?3,?4) ON CONFLICT(config) DO UPDATE SET streak=excluded.streak,next_at=excluded.next_at,blocked=excluded.blocked",params![self.config,self.streak,next,blocked])?;
        Ok(())
    }
    fn failed(&mut self, b: &Broker) -> Result<Slot> {
        self.streak = self
            .streak
            .checked_add(1)
            .ok_or("worker failure counter overflow")?;
        let delay = match self.streak {
            1 => 1000,
            2 => 2000,
            _ => 300000,
        };
        self.save(
            b,
            now()
                .checked_add(delay)
                .ok_or("worker retry timestamp overflow")?,
            None,
        )?;
        Ok(Slot::Waiting(
            Instant::now() + Duration::from_millis(delay as u64),
        ))
    }
}
fn start(
    port: &impl WorkerProcessPort,
    b: &mut Broker,
    c: &RuntimeConfig,
    events: &mpsc::Sender<Event>,
    signal: &Arc<Signal>,
) -> Result<Active> {
    let handle = b.reserve_worker_start(MODEL_ID, now())?;
    if !handle.should_start {
        return Err("supervisor start reservation already owned".into());
    }
    let inc = handle.incarnation;
    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let cached = b
        .connection
        .query_row(
            "SELECT fingerprint,report FROM execution_selection ORDER BY rowid DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
        .and_then(|(f, j)| serde_json::from_str(&j).ok().map(|v| (f, v)));
    let mut child = OwnedChild(port.spawn(c)?);
    b.connection.execute("INSERT INTO diagnostics VALUES('worker_process',?1) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",[serde_json::json!({"pid":child.id(),"incarnation":inc}).to_string()])?;
    if let Err(e) = port.observe(&child, inc.clone(), events.clone(), signal.clone()) {
        let _ = port.stop(&mut child);
        return Err(e);
    }
    let launch = Launch {
        incarnation: inc.clone(),
        secret: secret.clone(),
        state: c.state_dir.clone(),
        assets: c.model.clone(),
        cached,
    };
    if let Err(e) = model_child::write(child.stdin.as_mut().ok_or("missing child input")?, &launch)
    {
        let _ = port.stop(&mut child);
        return Err(e);
    }
    let mut output = child.stdout.take().ok_or("missing child output")?;
    let events = events.clone();
    let signal = signal.clone();
    let observed = inc.clone();
    std::thread::spawn(move || loop {
        match model_child::read::<Output>(&mut output) {
            Ok(message) => {
                if events
                    .send(Event::Output(observed.clone(), message))
                    .is_err()
                {
                    break;
                }
                signal.notify();
            }
            Err(e) => {
                let _ = events.send(Event::Pipe(observed, e.to_string()));
                signal.notify();
                break;
            }
        }
    });
    b.transition(&inc, WorkerState::Loading, now())?;
    Ok(Active {
        child,
        inc,
        secret,
        control: None,
        job: None,
        ready_since: None,
        last_control: Instant::now(),
        unavailable: false,
        exited: false,
    })
}
/// Read-only doctor snapshot. Never attaches, launches, mutates schema, or exposes credentials/payloads.
pub fn inspect_runtime(state: &std::path::Path) -> Result<serde_json::Value> {
    let path = state.join("broker.sqlite");
    if !path.exists() {
        return Ok(serde_json::json!({"present":false}));
    }
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(2))?;
    db.execute_batch("BEGIN")?;
    let has = |table: &str| -> rusqlite::Result<bool> {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )
    };
    let mut result = serde_json::json!({"present":true});
    if has("worker")? {
        result["worker"]=db.query_row("SELECT incarnation,state,model,heartbeat FROM worker",[],|r|Ok(serde_json::json!({"incarnation":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"model":r.get::<_,String>(2)?,"broker_heartbeat_ms":r.get::<_,i64>(3)?})))?;
    }
    if has("worker_recovery")? {
        let mut s=db.prepare("SELECT config,streak,next_at,blocked FROM worker_recovery ORDER BY next_at DESC LIMIT 64")?;
        result["recovery"]=s.query_map([],|r|Ok(serde_json::json!({"configuration":r.get::<_,String>(0)?,"failure_streak":r.get::<_,i64>(1)?,"next_restart_ms":r.get::<_,i64>(2)?,"startup_blocked":r.get::<_,Option<String>>(3)?.is_some()})))?.collect::<rusqlite::Result<Vec<_>>>()?.into();
    }
    if has("jobs")? {
        let mut s = db.prepare("SELECT state,durable,COUNT(*) FROM jobs GROUP BY state,durable")?;
        result["queue"]=s.query_map([],|r|Ok(serde_json::json!({"state":r.get::<_,String>(0)?,"durable":r.get::<_,bool>(1)?,"count":r.get::<_,i64>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?.into();
    }
    if has("diagnostics")? {
        let mut s=db.prepare("SELECT name,cause FROM diagnostics WHERE name IN('worker_supervision','worker_recovery_state','worker_process','worker_resources','model_load','supervisor_fatal','execution_selection','index_background','index_background_status') OR name LIKE 'vector_index:%' ORDER BY name LIMIT 128")?;
        result["diagnostics"] = s
            .query_map([], |r| {
                Ok(serde_json::json!({"name":r.get::<_,String>(0)?,"detail":r.get::<_,String>(1)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into();
    }
    if has("generation_ack")? {
        let mut s=db.prepare("SELECT repository,channel,epoch,sequence,index_commit_id FROM generation_ack ORDER BY repository,channel LIMIT 128")?;
        result["generation_ack_first_128"]=s.query_map([],|r|Ok(serde_json::json!({"repository":r.get::<_,String>(0)?,"channel":r.get::<_,String>(1)?,"epoch":r.get::<_,String>(2)?,"sequence":r.get::<_,i64>(3)?,"commit":r.get::<_,i64>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?.into();
    }
    db.execute_batch("ROLLBACK")?;
    Ok(result)
}
pub(crate) fn run(c: RuntimeConfig, stop: Arc<AtomicBool>, signal: Arc<Signal>) -> Result<()> {
    run_with_port(c, stop, signal, NativeProcess)
}
fn initialize(b: &Broker) -> Result<()> {
    b.connection.execute_batch("CREATE TABLE IF NOT EXISTS diagnostics(name TEXT PRIMARY KEY,cause TEXT NOT NULL);CREATE TABLE IF NOT EXISTS execution_selection(fingerprint TEXT PRIMARY KEY,report TEXT NOT NULL);")?;
    b.connection.execute_batch("CREATE TABLE IF NOT EXISTS worker_recovery(config TEXT PRIMARY KEY,streak INTEGER NOT NULL,next_at INTEGER NOT NULL,blocked TEXT);CREATE TABLE IF NOT EXISTS worker_model_configs(config TEXT PRIMARY KEY,model TEXT NOT NULL);CREATE TABLE IF NOT EXISTS waiter_failures(job INTEGER NOT NULL,lease TEXT NOT NULL,cause TEXT NOT NULL,PRIMARY KEY(job,lease));")?;
    Ok(())
}
// A private OS-boundary seam: tests still execute the production supervisor,
// native child, SQLite journal and IPC. No test command or MCP action is exposed.
fn run_with_port(
    c: RuntimeConfig,
    stop: Arc<AtomicBool>,
    signal: Arc<Signal>,
    port: impl WorkerProcessPort,
) -> Result<()> {
    let mut b = Broker::open(c.state_dir.join("broker.sqlite"), 64)?;
    initialize(&b)?;
    let old: String = b
        .connection
        .query_row("SELECT incarnation FROM worker", [], |r| r.get(0))?;
    lose_claims(
        &mut b,
        &old,
        "previous broker ownership ended; unfinished model claims recovered",
    )?;
    let config = format!("{:x}", Sha256::digest(serde_json::to_vec(&c.model)?));
    let (mut recovery, mut slot) = Recovery::load(&b, config)?;
    let (tx, rx) = mpsc::channel();
    loop {
        let observed = signal.version();
        if stop.load(Ordering::Relaxed) {
            if let Slot::Active(mut active) = slot {
                b.connection.execute(
                    "UPDATE worker SET state='stopping' WHERE incarnation=?1",
                    [&active.inc],
                )?;
                if let Some(address) = &active.control {
                    let _ = port.control(address, &active.inc, &active.secret, true);
                }
                port.stop(&mut active.child)?;
            }
            b.connection
                .execute("UPDATE worker SET state='stopped'", [])?;
            return Ok(());
        }
        for event in rx.try_iter() {
            let Slot::Active(active) = &mut slot else {
                continue;
            };
            let inc = match &event {
                Event::Output(i, _)
                | Event::Exit(i)
                | Event::Pipe(i, _)
                | Event::ControlLost(i, _) => i,
            };
            if inc != &active.inc {
                continue;
            }
            match event {
                Event::Output(
                    _,
                    Output::Hello {
                        incarnation,
                        control,
                    },
                ) => {
                    if incarnation != active.inc {
                        let _ = tx.send(Event::Pipe(
                            active.inc.clone(),
                            "model handshake incarnation mismatch".into(),
                        ));
                        signal.notify();
                        continue;
                    }
                    active.control = Some(control);
                    if let Err(error) = heartbeat(&port, &b, active) {
                        let _ = tx.send(Event::Pipe(active.inc.clone(), error.to_string()));
                        signal.notify();
                        continue;
                    }
                }
                Event::Output(_, Output::Ready { model }) => {
                    b.connection.execute("INSERT INTO worker_model_configs VALUES(?1,?2) ON CONFLICT(config) DO UPDATE SET model=excluded.model",params![recovery.config,model])?;
                    b.connection.execute(
                        "UPDATE worker SET model=?1 WHERE incarnation=?2",
                        params![model, active.inc],
                    )?;
                    b.transition(&active.inc, WorkerState::Ready, now())?;
                    active.ready_since = Some(Instant::now());
                    if let Err(error) = heartbeat(&port, &b, active) {
                        let _ = tx.send(Event::ControlLost(active.inc.clone(), error.to_string()));
                    }
                    diagnostic(
                        &b,
                        "worker_recovery_state",
                        "READY; previous failure diagnostic retained",
                    )?;
                    signal.notify();
                }
                Event::Output(_, Output::Unavailable { cause }) => {
                    active.unavailable = true;
                    diagnostic(&b, "model_load", &cause)?;
                    lose_claims(&mut b, &active.inc, &cause)?;
                    let assets = fingerprint(&c)?;
                    recovery.save(&b, 0, Some(&assets))?;
                    signal.notify();
                }
                Event::Output(
                    _,
                    Output::Selection {
                        fingerprint,
                        selection,
                    },
                ) => {
                    diagnostic(
                        &b,
                        "execution_selection",
                        &serde_json::to_string(&selection)?,
                    )?;
                    if let Some(key) = fingerprint.filter(|_| selection.cacheable) {
                        b.connection.execute("INSERT INTO execution_selection VALUES(?1,?2) ON CONFLICT(fingerprint) DO UPDATE SET report=excluded.report",params![key,serde_json::to_string(&selection)?])?;
                    }
                }
                Event::Output(
                    _,
                    Output::Result {
                        id,
                        attempt,
                        result,
                    },
                ) => {
                    let Some(job) = active
                        .job
                        .as_ref()
                        .filter(|j| j.id == id && j.attempts == attempt)
                    else {
                        continue;
                    };
                    match result {
                        Ok(vector) => match b.complete(job, &vector) {
                            Ok(()) => {
                                b.connection.execute("UPDATE jobs SET state='acknowledged',payload='' WHERE id=?1 AND durable=0",[id])?;
                            }
                            Err(crate::Error::InvalidState) => {
                                b.fail(job, "model returned invalid result", false, now())?
                            }
                            Err(crate::Error::ObsoleteIncarnation) => {}
                            Err(error) => return Err(error.into()),
                        },
                        Err(e) => b.fail(
                            job,
                            &e.to_string(),
                            matches!(e, EmbeddingError::Retryable(_)) && job.attempts < 8,
                            now(),
                        )?,
                    }
                    active.job = None;
                    signal.notify();
                }
                Event::ControlLost(_, cause) => {
                    if heartbeat(&port, &b, active).is_ok() {
                        continue;
                    }
                    let _ = tx.send(Event::Pipe(active.inc.clone(), cause));
                    signal.notify();
                }
                Event::Exit(_) => {
                    active.exited = true; /* Drain ordered stdout results/diagnostics before handling its EOF. */
                }
                Event::Pipe(_, _) => {
                    let Slot::Active(mut ended) = std::mem::replace(&mut slot, Slot::Stopped)
                    else {
                        unreachable!()
                    };
                    let cause = match event {
                        Event::Pipe(_, cause) => format!("model IPC ended: {cause}"),
                        _ => unreachable!(),
                    };
                    // EOF alone is not proof of death. The exact owned handle is checked,
                    // authenticated control is retried, then owned stop+wait must succeed.
                    if ended.child.try_wait()?.is_none() {
                        if let Some(address) = &ended.control {
                            let _ = port.control(address, &ended.inc, &ended.secret, false);
                            let _ = port.control(address, &ended.inc, &ended.secret, true);
                        }
                        port.stop(&mut ended.child)?;
                    } else {
                        ended.child.wait()?;
                    }
                    if ended.unavailable {
                        slot = Slot::Blocked(fingerprint(&c)?);
                    } else {
                        lose_claims(&mut b, &ended.inc, &cause)?;
                        slot = recovery.failed(&b)?;
                    }
                    signal.notify();
                }
            }
        }
        if matches!(slot, Slot::Active(_))
            && b.idle_shutdown_due(now(), 600000)?
            && !demand(&b, &recovery.config)?
        {
            let Slot::Active(mut idle) = std::mem::replace(&mut slot, Slot::Stopped) else {
                unreachable!()
            };
            b.connection.execute(
                "UPDATE worker SET state='stopping' WHERE incarnation=?1",
                [&idle.inc],
            )?;
            if let Some(address) = &idle.control {
                let _ = port.control(address, &idle.inc, &idle.secret, true);
            }
            port.stop(&mut idle.child)?; // No reservation until exact owned termination completes.
            b.connection.execute(
                "UPDATE worker SET state='stopped' WHERE incarnation=?1",
                [&idle.inc],
            )?;
            diagnostic(
                &b,
                "worker_recovery_state",
                "STOPPED after idle grace; broker remains available",
            )?;
            signal.notify();
        }
        match &mut slot {
            Slot::Active(active) => {
                if !active.exited && active.last_control.elapsed() >= Duration::from_secs(10) {
                    if active.control.is_some() {
                        if let Err(error) = heartbeat(&port, &b, active) {
                            let _ = tx.send(Event::ControlLost(
                                active.inc.clone(),
                                format!("control reconnect failed: {error}"),
                            ));
                            signal.notify();
                        }
                    }
                }
                if active
                    .ready_since
                    .is_some_and(|since| since.elapsed() >= Duration::from_secs(60))
                    && recovery.streak != 0
                    && active.last_control.elapsed() < Duration::from_secs(10)
                {
                    recovery.streak = 0;
                    recovery.save(&b, 0, None)?;
                }
                if active.ready_since.is_some()
                    && active.job.is_none()
                    && !active.unavailable
                    && !active.exited
                {
                    if let Some(job) = b.claim(&active.inc, now())? {
                        let bytes = match model_child::encode(&Input::Embed {
                            id: job.id,
                            attempt: job.attempts,
                            text: job.payload.clone(),
                        }) {
                            Ok(bytes) => bytes,
                            Err(error) => {
                                b.fail(
                                    &job,
                                    &format!("job serialization rejected: {error}"),
                                    false,
                                    now(),
                                )?;
                                signal.notify();
                                continue;
                            }
                        };
                        let input = active.child.stdin.as_mut().ok_or("child input closed")?;
                        let result = input.write_all(&bytes).and_then(|_| input.flush());
                        active.job = Some(job);
                        if let Err(error) = result {
                            let _ = tx.send(Event::Pipe(active.inc.clone(), error.to_string()));
                            signal.notify();
                        }
                    }
                }
            }
            Slot::Blocked(assets) => {
                if fingerprint(&c)? != *assets {
                    recovery.save(&b, 0, None)?;
                    slot = Slot::Waiting(Instant::now());
                }
            }
            Slot::Stopped => {
                if demand(&b, &recovery.config)? {
                    slot = Slot::Waiting(Instant::now());
                }
            }
            Slot::Waiting(until) => {
                if Instant::now() >= *until && demand(&b, &recovery.config)? {
                    let lock = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .open(c.state_dir.join("model.lock"))?;
                    if lock.try_lock_exclusive().is_ok() {
                        FileExt::unlock(&lock)?;
                        match start(&port, &mut b, &c, &tx, &signal) {
                            Ok(active) => slot = Slot::Active(active),
                            Err(error) => {
                                diagnostic(&b, "model_load", &error.to_string())?;
                                b.connection
                                    .execute("UPDATE worker SET state='failed'", [])?;
                                slot = recovery.failed(&b)?;
                            }
                        }
                    } else {
                        diagnostic(&b,"worker_supervision","incumbent model lock remains live; spawn withheld until owned orphan exits")?;
                        *until = Instant::now() + Duration::from_secs(5);
                    }
                }
            }
        }
        // Work/control/retry clocks wake the same state owner; process exit itself uses an OS wait.
        let mut wait = Duration::from_secs(10);
        if let Slot::Waiting(until) = &slot {
            if demand(&b, &recovery.config)? {
                wait = wait.min(until.saturating_duration_since(Instant::now()));
            }
        }
        if let Slot::Active(active) = &slot {
            if active.control.is_some() && !active.exited {
                wait =
                    wait.min(Duration::from_secs(10).saturating_sub(active.last_control.elapsed()));
            }
            if active.job.is_none() && active.ready_since.is_some() {
                let next:Option<i64>=b.connection.query_row("SELECT MIN(next_attempt) FROM jobs WHERE state='retry_wait' AND model=(SELECT model FROM worker)",[],|r|r.get(0))?;
                if let Some(next) = next {
                    wait = wait.min(Duration::from_millis(
                        next.saturating_sub(now()).max(1) as u64
                    ));
                }
            }
        }
        signal.wait(observed, wait.max(Duration::from_millis(1)));
    }
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "supervisor_native_tests.rs"]
mod native_tests;
