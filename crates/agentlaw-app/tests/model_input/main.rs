#![cfg(windows)]

#[path = "../../../../tests/support/runtime_cleanup.rs"]
mod runtime_cleanup;

use agentlaw_worker::{EmbeddingProvider, ModelAssets, OnnxProvider};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    os::windows::{io::AsRawHandle, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, INVALID_HANDLE_VALUE, WAIT_TIMEOUT},
    System::{
        JobObjects::*,
        SystemServices::{JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT, JOB_OBJECT_QUERY},
        Threading::GetCurrentProcess,
        IO::{CreateIoCompletionPort, GetQueuedCompletionStatus},
    },
};

const JOB_LIMIT: usize = 12 * 1024 * 1024 * 1024;
const CHILD_TEST: &str = "real_model_caps_long_inputs_in_a_bounded_cpu_process";

const MODEL_SHA: &str = "f9defdceaaae9d4d4007ad601b05b6e436375e7293e52b74ed1f7a3933c5b26a";
const TOKENIZER_SHA: &str = "0087c868b33bad550a78a08d19798cfd7f713cde4f020803b8f51f405503e15f";
const ORT_SHA: &str = "579b636403983254346a5c1d80bd28f1519cd1e284cd204f8d4ff41f8d711559";
const BODY_SHA: &str = "f69cd5c5ab18b44c226033279bc3dc8af6de910dba3453111ffddfdeb3069772";

fn verified_copy(root: &Path, var: &str, name: &str, expected: &str) -> PathBuf {
    let source = asset(var);
    assert_eq!(sha256(&source), expected, "{var}");
    let target = root.join(name);
    std::fs::copy(source, &target).unwrap();
    assert_eq!(sha256(&target), expected, "isolated {name}");
    target
}

fn require_test_job() {
    let name: Vec<u16> = std::env::var("AGENTLAW_MODEL_INPUT_JOB_NAME")
        .unwrap()
        .encode_utf16()
        .chain([0])
        .collect();
    let job = OwnedHandle(unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, name.as_ptr()) });
    assert!(!job.0.is_null(), "open the specific test-owned Job");
    let mut member = 0;
    assert_ne!(
        unsafe { IsProcessInJob(GetCurrentProcess(), job.0, &mut member) },
        0
    );
    assert_ne!(member, 0, "current process is outside the test-owned Job");
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    assert_ne!(
        unsafe {
            QueryInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&mut info as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&info) as u32,
                std::ptr::null_mut(),
            )
        },
        0
    );
    let flags = JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    assert_eq!(info.BasicLimitInformation.LimitFlags & flags, flags);
    assert_eq!(info.ProcessMemoryLimit, JOB_LIMIT);
}

fn asset(var: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(var).unwrap_or_else(|| panic!("set {var}")))
        .canonicalize()
        .unwrap()
}

fn sha256(path: &Path) -> String {
    let mut file = std::fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buf = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buf).unwrap();
        if count == 0 {
            return format!("{:x}", hash.finalize());
        }
        hash.update(&buf[..count]);
    }
}

struct OwnedHandle(windows_sys::Win32::Foundation::HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn run(state: &Path, args: &[&str], input: Option<Value>) -> Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agentlaw"));
    let mut child = cmd
        .args(args)
        .env("AGENTLAW_HOME", state)
        .env_remove("AGENTLAW_ONNX_MODEL")
        .env_remove("AGENTLAW_TOKENIZER")
        .env_remove("AGENTLAW_ORT_LIBRARY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000)
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn diagnostics(value: &Value, code: &str) -> usize {
    value["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|diagnostic| diagnostic["code"] == code)
        .count()
}

fn check_model() {
    let mut gate = [0];
    std::io::stdin().read_exact(&mut gate).unwrap();
    assert_eq!(gate, [1]);
    require_test_job();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("fixtures");
    std::fs::create_dir(&root).unwrap();
    let assets = ModelAssets {
        onnx_model: verified_copy(&root, "AGENTLAW_ONNX_MODEL", "model.onnx", MODEL_SHA),
        tokenizer_json: verified_copy(&root, "AGENTLAW_TOKENIZER", "tokenizer.json", TOKENIZER_SHA),
        runtime_library: verified_copy(&root, "AGENTLAW_ORT_LIBRARY", "onnxruntime.dll", ORT_SHA),
    };
    // The measurement recorded 74,881 raw tokens for these exact bytes.
    let body_path = verified_copy(&root, "AGENTLAW_DENSE_BODY", "A-dense-v1.txt", BODY_SHA);
    let body = std::fs::read_to_string(body_path).unwrap();
    assert_eq!(body.len(), 113_524);
    let provider = OnnxProvider::load(&assets).unwrap();
    let input = "Bound the model input before calling CPU inference.";
    let first = provider.embed(input).unwrap();
    let second = provider.embed(input).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.len(), 256);
    drop(provider);

    let state = tmp.path().join("state");
    let _cleanup = runtime_cleanup::RuntimeCleanup(state.clone());
    let harness = tmp.path().join("codex");
    let source = tmp.path().join("source");
    let manifest = tmp.path().join("assets.json");
    std::fs::write(
        &manifest,
        json!({
            "model_build_id":"granite-r2-256-portable-qdq-v1",
            "onnx_model":{"path":assets.onnx_model,"sha256":MODEL_SHA},
            "tokenizer_json":{"path":assets.tokenizer_json,"sha256":TOKENIZER_SHA},
            "runtime_library":{"path":assets.runtime_library,"sha256":ORT_SHA},
        })
        .to_string(),
    )
    .unwrap();
    let installed = run(
        &state,
        &[
            "install",
            "--harness",
            "codex",
            "--harness-dir",
            harness.to_str().unwrap(),
            "--model-manifest",
            manifest.to_str().unwrap(),
            "--confirm-install",
        ],
        None,
    );
    assert_eq!(installed["semantic_model_configured"], true);
    let created = run(
        &state,
        &[
            "store",
            "create",
            "--path",
            source.to_str().unwrap(),
            "--confirm-create",
        ],
        None,
    );
    assert_eq!(
        created["derived_build"]["semantic_complete"], true,
        "{created}"
    );
    let cache_path = state.join("worker").join("model-execution-selection.json");
    let deadline = Instant::now() + Duration::from_secs(30);
    let selection: Value = loop {
        if let Ok(bytes) = std::fs::read(&cache_path) {
            if let Ok(selection) = serde_json::from_slice(&bytes) {
                break selection;
            }
        }
        assert!(
            Instant::now() < deadline,
            "model device selection was not saved"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(selection[1]["backend"], "Cpu", "{selection}");
    let recall = run(
        &state,
        &["call", "--json", "-"],
        Some(json!({"action":"recall","recall":{"recall_for":"x ".repeat(32_768)}})),
    );
    assert_eq!(
        diagnostics(&recall, "semantic_query_truncated"),
        1,
        "{recall}"
    );
    assert_eq!(
        diagnostics(&recall, "semantic_channel_incomplete"),
        0,
        "{recall}"
    );

    let started = Instant::now();
    let saved = run(
        &state,
        &["call", "--json", "-"],
        Some(json!({"action":"remember_this","remember_this":{
            "memories":[{"operation":"create","what_to_remember":body,"applies_to":["user"],"evidence":"Isolated real-model input limit test"}]
        }})),
    );
    eprintln!("remember elapsed: {:?}", started.elapsed());
    assert_eq!(saved["status"], "remembered", "{saved}");
    assert!(
        !saved.to_string().contains("semantic_channel_incomplete"),
        "{saved}"
    );
    let memory_id = saved["results"][0]["memory_ref"]["memory_id"]
        .as_str()
        .unwrap();
    // Read the runtime's own binding; a fresh local folder is not bound to the existing source.
    let config = agentlaw_runtime::installation::config::load(&state)
        .unwrap()
        .expect("installed configuration");
    let binding = config.runtime_root(&state).join("canonical");
    let store = agentlaw_storage::Store::open_read_only_with_coordination(
        &source,
        binding,
        agentlaw_runtime::installation::config::coordination_root(&state),
    )
    .unwrap();
    let source_sequence = store
        .owned_published_reader()
        .read_snapshot()
        .unwrap()
        .position()
        .sequence;
    let deadline = Instant::now() + Duration::from_secs(300);
    let indices = state.join("worker").join("indices");
    loop {
        let complete = std::fs::read_dir(&indices).unwrap().flatten().any(|entry| {
            let path=entry.path().join("vector.sqlite");
            let Ok(conn)=rusqlite::Connection::open_with_flags(path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {return false;};
            conn.query_row("SELECT complete AND ack = ?2 AND EXISTS(SELECT 1 FROM vectors WHERE memory=?1) FROM binding_vector_meta WHERE kind='current'",rusqlite::params![memory_id,source_sequence as i64],|row|row.get::<_,bool>(0)).unwrap_or(false)
        });
        if complete {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "published memory did not reach complete semantic coverage"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "requires release Granite model, tokenizer and CPU ONNX Runtime via AGENTLAW_ONNX_MODEL, AGENTLAW_TOKENIZER and AGENTLAW_ORT_LIBRARY, plus the measured body via AGENTLAW_DENSE_BODY"]
fn real_model_caps_long_inputs_in_a_bounded_cpu_process() {
    if std::env::var_os("AGENTLAW_MODEL_INPUT_JOB_CHILD").is_some() {
        return check_model();
    }
    let name = format!("Local\\AgentlawModelInput-{}", uuid::Uuid::new_v4());
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let job = OwnedHandle(unsafe { CreateJobObjectW(std::ptr::null(), wide.as_ptr()) });
    assert!(!job.0.is_null());
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    limits.ProcessMemoryLimit = JOB_LIMIT;
    assert_ne!(
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        },
        0
    );
    let port = OwnedHandle(unsafe {
        CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1)
    });
    assert!(!port.0.is_null());
    let completion = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
        CompletionKey: job.0,
        CompletionPort: port.0,
    };
    assert_ne!(
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectAssociateCompletionPortInformation,
                (&completion as *const JOBOBJECT_ASSOCIATE_COMPLETION_PORT).cast(),
                std::mem::size_of_val(&completion) as u32,
            )
        },
        0
    );
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
        .env("AGENTLAW_MODEL_INPUT_JOB_CHILD", "1")
        .env("AGENTLAW_MODEL_INPUT_JOB_NAME", name)
        .stdin(Stdio::piped())
        .creation_flags(0x08000000)
        .spawn()
        .unwrap();
    if unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle()) } == 0 {
        let error = std::io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        panic!("failed to contain test-owned process: {error}");
    }
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(900);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "real-model test exceeded its observation deadline"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut memory_limit_events = 0;
    loop {
        let mut message = 0;
        let mut key = 0;
        let mut process = std::ptr::null_mut();
        let received =
            unsafe { GetQueuedCompletionStatus(port.0, &mut message, &mut key, &mut process, 0) };
        if received == 0 {
            assert_eq!(unsafe { GetLastError() }, WAIT_TIMEOUT);
            break;
        }
        assert_eq!(key, job.0 as usize);
        if message == JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT {
            memory_limit_events += 1;
        }
    }
    assert_eq!(memory_limit_events, 0);
    assert!(status.success(), "{status}");
    assert_ne!(
        unsafe {
            QueryInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
                std::ptr::null_mut(),
            )
        },
        0
    );
    eprintln!(
        "maximum per-process commit charge recorded by the test Job (bytes): {}",
        limits.PeakProcessMemoryUsed
    );
}
