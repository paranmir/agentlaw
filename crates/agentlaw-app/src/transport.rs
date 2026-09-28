//! One serialized Runtime lane, with independent input for progress/cancellation.
//! MCP 2025-06-18 progress/cancellation, not a claim of support for Tasks.
use crate::{
    error_payload, read_message, rpc_error, tool_result, update::Advisor, Backend, McpSession,
};
use agentlaw_contracts::{DomainError, Request, Result};
use agentlaw_flows::RequestControl;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{BufRead, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
};

struct Capture(Option<Request>);
impl Backend for Capture {
    fn call(&mut self, request: Request) -> Result<Value> {
        self.0 = Some(request);
        Ok(Value::Null)
    }
}
struct Work {
    id: Value,
    request: Request,
    cancel: Arc<AtomicBool>,
    token: Option<Value>,
}
fn io_error() -> DomainError {
    DomainError::new(
        "transport_io",
        "MCP transport failed; do not assume a submitted write was rolled back.",
    )
}
fn emit<W: Write>(writer: &Mutex<W>, value: &Value) -> Result<()> {
    let mut out = writer.lock().map_err(|_| io_error())?;
    serde_json::to_writer(&mut *out, value).map_err(|_| io_error())?;
    out.write_all(b"\n")
        .and_then(|_| out.flush())
        .map_err(|_| io_error())
}

pub fn serve<R: BufRead, W: Write + Send + 'static, B: Backend + Send>(
    reader: R,
    writer: W,
    backend: B,
) -> Result<()> {
    serve_with_advisor(reader, writer, backend, None)
}

pub fn serve_with_advisor<R: BufRead, W: Write + Send + 'static, B: Backend + Send>(
    mut reader: R,
    writer: W,
    mut backend: B,
    advisor: Option<Advisor>,
) -> Result<()> {
    let output = Arc::new(Mutex::new(writer));
    let active = Arc::new(Mutex::new(BTreeMap::<String, Arc<AtomicBool>>::new()));
    let (send, receive) = mpsc::sync_channel::<Work>(8);
    std::thread::scope(|scope| {
        let writes = output.clone();
        let running = active.clone();
        let worker=scope.spawn(move||->Result<()> {
            while let Ok(work)=receive.recv() {
                let writes=writes.clone();
                let progress_output=writes.clone();
                let counter=AtomicU64::new(0);
                let failed=Arc::new(AtomicBool::new(false));
                let progress_failed=failed.clone();
                let cancelled=work.cancel.clone();
                let token=work.token.clone();
                let last_phase=Mutex::new(None::<String>);
                let control=RequestControl::new(work.cancel.clone(),move|phase|{
                    // Repeated batch phases are coalesced. No invented percent or total.
                    let mut last=last_phase.lock().unwrap_or_else(|e|e.into_inner());
                    if last.as_deref()==Some(phase) || cancelled.load(Ordering::Acquire) {return;}
                    *last=Some(phase.to_owned());
                    if let Some(token)=&token {
                        let n=counter.fetch_add(1,Ordering::Relaxed)+1;
                        if emit(&progress_output,&json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":token,"progress":n,"message":phase}})).is_err() {
                            progress_failed.store(true,Ordering::Release);
                            cancelled.store(true,Ordering::Release);
                        }
                    }
                });
                let result=control.check().and_then(|_|backend.call_with_control(work.request,control));
                let _guard=running.lock().map_err(|_|io_error())?.remove(&work.id.to_string());
                if failed.load(Ordering::Acquire) {return Err(io_error());}
                // A cancellation after the durable decision cannot roll it back.
                // Complete recovery, then suppress the transport response as MCP recommends.
                if work.cancel.load(Ordering::Acquire) {continue;}
                let (body,is_error)=match result {Ok(v)=>(v,false),Err(e)=>(error_payload(&e),true)};
                let notice = advisor.as_ref().and_then(Advisor::notice);
                emit(&writes,&json!({"jsonrpc":"2.0","id":work.id,"result":tool_result(body,is_error,notice)}))?;
            }
            Ok(())
        });
        let mut session = McpSession::default();
        let input_result = (|| -> Result<()> {
            while let Some(line) = read_message(&mut reader)? {
                let parsed = agentlaw_contracts::validation::decode_unique(&line).ok();
                if let Some(v) = &parsed {
                    if v["jsonrpc"] == "2.0"
                        && v["method"] == "notifications/cancelled"
                        && v.get("id").is_none()
                    {
                        if let Some(id) = v["params"].get("requestId") {
                            if let Some(cancel) =
                                active.lock().map_err(|_| io_error())?.get(&id.to_string())
                            {
                                cancel.store(true, Ordering::Release);
                            }
                        }
                        continue;
                    }
                    if let Some(id) = v.get("id") {
                        if active
                            .lock()
                            .map_err(|_| io_error())?
                            .contains_key(&id.to_string())
                        {
                            emit(
                                &output,
                                &rpc_error(id.clone(), -32600, "Request ID is already in use."),
                            )?;
                            continue;
                        }
                    }
                }
                let mut capture = Capture(None);
                let response = session.handle(&line, &mut capture);
                if let Some(request) = capture.0 {
                    let value = parsed.expect("validated captured tool request");
                    let id = value["id"].clone();
                    let token = value["params"]["_meta"].get("progressToken").cloned();
                    if token
                        .as_ref()
                        .is_some_and(|v| !v.is_string() && !v.is_i64() && !v.is_u64())
                    {
                        emit(
                            &output,
                            &rpc_error(id, -32602, "progressToken must be a string or integer."),
                        )?;
                        continue;
                    }
                    let cancel = Arc::new(AtomicBool::new(false));
                    active
                        .lock()
                        .map_err(|_| io_error())?
                        .insert(id.to_string(), cancel.clone());
                    if let Err(e) = send.try_send(Work {
                        id: id.clone(),
                        request,
                        cancel,
                        token,
                    }) {
                        active
                            .lock()
                            .map_err(|_| io_error())?
                            .remove(&id.to_string());
                        match e {
                            mpsc::TrySendError::Full(_) => emit(
                                &output,
                                &rpc_error(
                                    id,
                                    -32001,
                                    "Request queue is full; this request was not executed.",
                                ),
                            )?,
                            mpsc::TrySendError::Disconnected(_) => return Err(io_error()),
                        }
                    }
                } else if let Some(response) = response {
                    emit(&output, &response)?;
                }
            }
            Ok(())
        })();
        if input_result.is_err() {
            for cancel in active.lock().map_err(|_| io_error())?.values() {
                cancel.store(true, Ordering::Release);
            }
        }
        drop(send);
        let work_result = worker.join().map_err(|_| {
            DomainError::new(
                "runtime_failed",
                "The request executor terminated unexpectedly; inspect retained recovery state.",
            )
        })?;
        input_result.and(work_result)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Read};
    struct Input {
        rx: mpsc::Receiver<Vec<u8>>,
        buf: Vec<u8>,
        pos: usize,
    }
    impl Read for Input {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let b = self.fill_buf()?;
            let n = b.len().min(out.len());
            out[..n].copy_from_slice(&b[..n]);
            self.consume(n);
            Ok(n)
        }
    }
    impl BufRead for Input {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.pos == self.buf.len() {
                self.buf = self.rx.recv().unwrap_or_default();
                self.pos = 0;
            }
            Ok(&self.buf[self.pos..])
        }
        fn consume(&mut self, n: usize) {
            self.pos += n;
        }
    }
    struct Output {
        tx: mpsc::Sender<Value>,
        buf: Vec<u8>,
    }
    impl Write for Output {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            for byte in b {
                if *byte == b'\n' {
                    let value = serde_json::from_slice(&self.buf).unwrap();
                    self.tx.send(value).unwrap();
                    self.buf.clear();
                } else {
                    self.buf.push(*byte);
                }
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    struct Waiting;
    impl Backend for Waiting {
        fn call(&mut self, _: Request) -> Result<Value> {
            unreachable!()
        }
        fn call_with_control(&mut self, _: Request, c: RequestControl) -> Result<Value> {
            c.phase("searching");
            while !c.cancel.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            c.check()?;
            Ok(json!({}))
        }
    }
    struct Immediate;
    impl Backend for Immediate {
        fn call(&mut self, _: Request) -> Result<Value> {
            Ok(json!({"status":"remembered","results":[]}))
        }
    }
    #[test]
    fn final_transport_result_preserves_memory_and_repeats_visible_advice() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("update-check.json"),
            format!(
                r#"{{"last_success":{},"retry_after":null,"latest_tag":"v99.0.0"}}"#,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            ),
        )
        .unwrap();
        let advisor = Advisor::new(root.path().to_path_buf());
        let lines = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","recall":{"recall_for":"test"}}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","recall":{"recall_for":"test"}}}}),
        ];
        let input = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        let (tx, rx) = mpsc::channel();
        serve_with_advisor(
            std::io::Cursor::new(input),
            Output { tx, buf: vec![] },
            Immediate,
            Some(advisor),
        )
        .unwrap();
        let responses: Vec<Value> = rx.try_iter().collect();
        assert_eq!(responses.len(), 3);
        for response in &responses[1..] {
            let result = &response["result"];
            assert_eq!(result["isError"], false);
            assert_eq!(result["structuredContent"]["status"], "remembered");
            assert_eq!(
                result["structuredContent"]["update_notice"]["latest_version"],
                "v99.0.0"
            );
            let text: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(text, result["structuredContent"]);
        }
    }
    #[test]
    fn progress_and_cancel_remain_live_while_tool_runs() {
        let (tx, rx) = mpsc::channel();
        let (out, observed) = mpsc::channel();
        let task = std::thread::spawn(move || {
            serve(
                Input {
                    rx,
                    buf: vec![],
                    pos: 0,
                },
                Output {
                    tx: out,
                    buf: vec![],
                },
                Waiting,
            )
        });
        let send = |v: Value| {
            tx.send(format!("{v}\n").into_bytes()).unwrap();
        };
        send(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{}}}),
        );
        assert_eq!(
            observed
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap()["id"],
            1
        );
        send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        send(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agentlaw","arguments":{"action":"recall","recall_for":"test"},"_meta":{"progressToken":"phase"}}}),
        );
        let progress = observed
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        assert_eq!(progress["method"], "notifications/progress");
        assert_eq!(progress["params"]["progressToken"], "phase");
        send(json!({"jsonrpc":"2.0","id":3,"method":"ping"}));
        assert_eq!(
            observed
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap()["id"],
            3
        );
        send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}));
        drop(tx);
        task.join().unwrap().unwrap();
        assert!(observed.try_recv().is_err());
    }
}
