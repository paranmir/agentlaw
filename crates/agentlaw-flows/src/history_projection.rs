//! Causal history selection over acquired immutable source components.
use crate::{history_diff, DomainError, HistoryRequest, RequestControl};
use agentlaw_storage::{
    history_spool::{Component, HistorySpool},
    Store,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
type Work<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const HISTORY_WORKSPACE_MEMORY_LIMIT: u64 = 64 * 1024 * 1024;
const HISTORY_ASSEMBLY_WORKSPACE_RESERVE: u64 = 16 * 1024 * 1024;
fn inline_assembly_fits(input: &mut impl Read, control: &RequestControl) -> Work<bool> {
    let mut bytes = [0u8; 32 * 1024];
    let (
        mut objects,
        mut arrays,
        mut strings,
        mut numbers,
        mut literals,
        mut fields,
        mut string_bytes,
    ) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut in_string, mut escaped, mut in_number) = (false, false, false);
    // Each map node is bounded as an internal BTreeMap node with all 11 key/value slots and 12 child pointers.
    let map_node = (11 * (std::mem::size_of::<String>() + std::mem::size_of::<Value>())
        + 16 * std::mem::size_of::<usize>()) as u64;
    loop {
        control.check()?;
        let n = input.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        for &byte in &bytes[..n] {
            if in_string {
                if escaped {
                    escaped = false;
                    string_bytes = string_bytes.saturating_add(1);
                } else if byte == b'\\' {
                    escaped = true;
                    string_bytes = string_bytes.saturating_add(1);
                } else if byte == b'"' {
                    in_string = false;
                } else {
                    string_bytes = string_bytes.saturating_add(1);
                }
                continue;
            }
            if in_number {
                if matches!(byte, b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
                    continue;
                }
                in_number = false;
            }
            match byte {
                b'"' => {
                    strings = strings.saturating_add(1);
                    in_string = true;
                }
                b'{' => objects = objects.saturating_add(1),
                b'[' => arrays = arrays.saturating_add(1),
                b':' => fields = fields.saturating_add(1),
                b'-' | b'0'..=b'9' => {
                    numbers = numbers.saturating_add(1);
                    in_number = true;
                }
                b't' | b'f' | b'n' => literals = literals.saturating_add(1),
                _ => {}
            }
        }
        let values = objects
            .saturating_add(arrays)
            .saturating_add(strings)
            .saturating_add(numbers)
            .saturating_add(literals);
        let estimate = HISTORY_ASSEMBLY_WORKSPACE_RESERVE
            .saturating_add(string_bytes.saturating_mul(2))
            .saturating_add(objects.saturating_add(fields).saturating_mul(map_node))
            .saturating_add(values.saturating_mul(2 * std::mem::size_of::<Value>() as u64));
        if estimate > HISTORY_WORKSPACE_MEMORY_LIMIT {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The connection is dropped before its owned directory, including on error.
struct ProjectionWorkspace {
    db: Connection,
    directory: Option<tempfile::TempDir>,
}
impl ProjectionWorkspace {
    fn for_spool(
        local: &Path,
        spool: &mut HistorySpool,
        identity: &str,
        request: &HistoryRequest,
        control: &RequestControl,
    ) -> Work<Self> {
        // Bound the projected SQLite pages, token postings, diff line references,
        // JSON escaping and copies, in addition to the acquired source pages.
        // Saturation deliberately selects the file-backed path on overflow.
        let source_bytes = spool.size_bytes()?;
        if source_bytes > HISTORY_WORKSPACE_MEMORY_LIMIT / 4 {
            spool.spill_to_file()?;
        }
        let query_bytes = request
            .history_for
            .as_ref()
            .map_or(0, |query| query.len() as u64);
        let mut estimate = source_bytes
            .saturating_add(8 * 1024 * 1024)
            .saturating_add(query_bytes.saturating_mul(256));
        let windows = if request.history_for.is_some() {
            u64::from(request.max_matches.unwrap_or(1)).saturating_add(1)
        } else {
            1
        };
        spool.visit_metadata_closure(identity, |change| {
            let result = (|| -> Work<()> {
                control.check()?;
                let metadata = serde_json::to_vec(&change.metadata)?.len() as u64;
                let cost = (256u64 * 1024)
                    .saturating_add(metadata.saturating_mul(8))
                    .saturating_add(
                        change
                            .body
                            .bytes
                            .saturating_add(change.evidence.bytes)
                            .saturating_mul(256),
                    );
                estimate = estimate.saturating_add(cost.saturating_mul(windows));
                Ok(())
            })();
            result.map_err(|e| agentlaw_storage::Error::Io(std::io::Error::other(e.to_string())))
        })?;
        if estimate <= HISTORY_WORKSPACE_MEMORY_LIMIT {
            let db = Connection::open_in_memory()?;
            db.execute_batch(
                "PRAGMA temp_store=MEMORY; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;",
            )?;
            Ok(Self {
                db,
                directory: None,
            })
        } else {
            spool.spill_to_file()?;
            let root = local.join("canonical").join("history-spools");
            std::fs::create_dir_all(&root)?;
            let directory = tempfile::tempdir_in(root)?;
            let db = Connection::open(directory.path().join("projection.sqlite"))?;
            db.execute_batch(
                "PRAGMA cache_size=-2048; PRAGMA temp_store=FILE; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;",
            )?;
            Ok(Self {
                db,
                directory: Some(directory),
            })
        }
    }
}

fn failure() -> DomainError {
    DomainError::new("history_projection_failed", "History could not be acquired or projected completely. Original memory is unchanged; use doctor to investigate source integrity and retry. This is not an empty history result.")
}
struct NoticeContext<'a> {
    config_file: Option<&'a Path>,
    general: bool,
}
fn history_notice(limit: usize, context: Option<&NoticeContext<'_>>) -> Value {
    let general = context.is_some_and(|context| context.general);
    let key = if general {
        "response_limit_bytes"
    } else {
        "history.response_limit_bytes"
    };
    let path = context
        .and_then(|context| context.config_file)
        .map(|path| json!(path))
        .unwrap_or_else(|| {
            json!("Use agentlaw config path to obtain the installation configuration file.")
        });
    json!({"applied_bytes":limit,"config_key":key,"config_file":path,"user_notice":format!("History details exceed the configured response size. Adjust {key} to change this limit.")})
}
fn history_artifact(local: &Path, input: &mut impl Read, notice: Value) -> Work<Value> {
    let mut out = crate::artifacts::ArtifactWriter::create(local.join("response-artifacts"))?;
    let mut pending = None;
    let mut buffer = [0u8; 32768];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if let Some(byte) = pending {
            out.write_all(&[byte])?;
        }
        out.write_all(&buffer[..n - 1])?;
        pending = Some(buffer[n - 1]);
    }
    if pending != Some(b'}') {
        return Err("invalid history result".into());
    }
    out.write_all(b",\"response_limit\":")?;
    serde_json::to_writer(&mut out, &notice)?;
    out.write_all(b"}")?;
    let artifact = out.finish()?;
    Ok(
        json!({"code":"complete_content_in_file","content_read":false,"complete":true,"artifact":artifact,"response_limit_bytes":notice["applied_bytes"],"response_limit":notice,"next_action":"The complete version-frozen history response is in the artifact and has not been read. Use the harness file-reading tool or shell to read that path in bounded sections before relying on it. If that environment cannot access the path, tell the user what is unread and ask for file transfer or adjust the indicated response limit; do not claim the history was restored. After the indicated retention period it may be cleaned automatically; a new history query reads the then-current source, not necessarily this version."}),
    )
}

fn candidates_in_history(
    db: &Connection,
    hits: Vec<agentlaw_search::Hit>,
) -> Work<Vec<agentlaw_search::Hit>> {
    let mut selected = Vec::new();
    let mut exists = db.prepare_cached("SELECT 1 FROM nodes WHERE id=?1")?;
    for mut hit in hits {
        if exists
            .query_row([&hit.change_id], |row| row.get::<_, i64>(0))
            .optional()?
            .is_some()
        {
            hit.memory_id = hit.change_id.clone();
            selected.push(hit);
        }
    }
    Ok(selected)
}
fn component(db: &Connection, id: &str, column: &str) -> Work<Component> {
    // column is exclusively one of the internal literal names, never user input.
    let encoded: String = db.query_row(
        &format!("SELECT {column} FROM nodes WHERE id=?1"),
        [id],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&encoded)?)
}
fn select_ancestors(db: &Connection, window: i64, layers: u32) -> Work<()> {
    db.execute_batch("DROP TABLE IF EXISTS temp.frontier; DROP TABLE IF EXISTS temp.next; CREATE TEMP TABLE frontier(id TEXT PRIMARY KEY); CREATE TEMP TABLE next(id TEXT PRIMARY KEY);")?;
    db.execute(
        "INSERT INTO frontier SELECT id FROM selected WHERE window=?1",
        [window],
    )?;
    for _ in 0..layers {
        db.execute_batch("DELETE FROM next;")?;
        db.execute("INSERT OR IGNORE INTO next SELECT e.parent FROM edges e JOIN frontier f ON f.id=e.child WHERE NOT EXISTS(SELECT 1 FROM selected s WHERE s.window=?1 AND s.id=e.parent)",[window])?;
        if db.query_row("SELECT count(*) FROM next", [], |r| r.get::<_, u64>(0))? == 0 {
            break;
        }
        db.execute("INSERT INTO selected SELECT ?1,id,0 FROM next", [window])?;
        db.execute_batch("DELETE FROM frontier; INSERT INTO frontier SELECT id FROM next;")?;
    }
    Ok(())
}
fn write_change(
    db: &Connection,
    id: &str,
    matched: bool,
    dir: Option<&Path>,
    out: &mut impl Write,
) -> Work<()> {
    let raw: String = db.query_row("SELECT metadata FROM nodes WHERE id=?1", [id], |r| r.get(0))?;
    let metadata: Value = serde_json::from_str(&raw)?;
    let parents = metadata["parent_change_ids"]
        .as_array()
        .ok_or("invalid parents")?;
    let base = if parents.len() > 1 {
        metadata["delta_base_id"].as_str()
    } else {
        parents.first().and_then(Value::as_str)
    };
    let millis = metadata["metadata_after"]["recorded_at_ms"]
        .as_i64()
        .ok_or("invalid timestamp")?;
    let recorded_at =
        time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(millis) * 1_000_000)?
            .format(&time::format_description::well_known::Rfc3339)?;
    write!(out, "{{\"change_id\":")?;
    serde_json::to_writer(&mut *out, id)?;
    write!(out, ",\"parent_change_ids\":")?;
    serde_json::to_writer(&mut *out, parents)?;
    write!(out, ",\"recorded_at\":")?;
    serde_json::to_writer(&mut *out, &recorded_at)?;
    write!(out, ",\"evidence\":")?;
    history_diff::json_string(&mut component(db, id, "evidence")?.open()?, out)?;
    write!(out, ",\"markdown_diff\":\"")?;
    if let Some(dir) = dir {
        let scratch = tempfile::tempdir_in(dir)?;
        let before = scratch.path().join("before");
        let after = scratch.path().join("after");
        if let Some(base) = base {
            let body = component(db, base, "body")?;
            if std::io::copy(&mut body.open()?, &mut File::create(&before)?)? != body.bytes {
                return Err("truncated body".into());
            }
        } else {
            File::create(&before)?;
        }
        let body = component(db, id, "body")?;
        if std::io::copy(&mut body.open()?, &mut File::create(&after)?)? != body.bytes {
            return Err("truncated body".into());
        }
        history_diff::write_diff(
            &before,
            &after,
            &scratch.path().join("lines.sqlite"),
            &mut history_diff::JsonEscape(out),
        )?;
    } else {
        let mut before = String::new();
        if let Some(base) = base {
            component(db, base, "body")?
                .open()?
                .read_to_string(&mut before)?;
        }
        let mut after = String::new();
        component(db, id, "body")?
            .open()?
            .read_to_string(&mut after)?;
        history_diff::JsonEscape(&mut *out)
            .write_all(crate::history::full_diff(&before, &after).as_bytes())?;
    }
    write!(out, "\"")?;
    if parents.len() > 1 {
        write!(out, ",\"delta_base_id\":")?;
        serde_json::to_writer(&mut *out, &base)?;
    }
    if matched {
        write!(out, ",\"matched\":true")?;
    }
    write!(out, "}}")?;
    Ok(())
}
fn write_map(db: &Connection, out: &mut impl Write) -> Work<()> {
    // Distinguish parallel paths: collapsing a diamond into two identical a→d
    // paths would hide which branch the caller can request.
    db.execute_batch("UPDATE nodes SET keynode=1 WHERE id IN(SELECT e.child FROM edges e JOIN nodes p ON p.id=e.parent WHERE p.children>1);")?;
    write!(out, "{{\"nodes\":[")?;
    let mut statement =
        db.prepare("SELECT id,parents,children FROM nodes WHERE keynode=1 ORDER BY ordinal")?;
    let mut rows = statement.query([])?;
    let mut comma = false;
    while let Some(row) = rows.next()? {
        if comma {
            write!(out, ",")?;
        }
        comma = true;
        let id: String = row.get(0)?;
        let parents: u64 = row.get(1)?;
        let children: u64 = row.get(2)?;
        let mut roles = Vec::new();
        if parents == 0 {
            roles.push("root");
        }
        if children == 0 {
            roles.push("head");
        }
        if children > 1 {
            roles.push("fork");
        }
        if parents > 1 {
            roles.push("integration");
        }
        serde_json::to_writer(&mut *out, &json!({"change_id":id,"roles":roles}))?;
    }
    write!(out, "],\"paths\":[")?;
    // Every non-key node has exactly one incoming and one outgoing edge. The
    // recursion compresses only those chains; branches and every parent survive.
    let mut statement=db.prepare("WITH RECURSIVE paths(start,end,count) AS (SELECT e.parent,e.child,0 FROM edges e JOIN nodes n ON n.id=e.parent WHERE n.keynode=1 UNION ALL SELECT p.start,e.child,p.count+1 FROM paths p JOIN nodes n ON n.id=p.end JOIN edges e ON e.parent=p.end WHERE n.keynode=0) SELECT p.start,p.end,p.count FROM paths p JOIN nodes n ON n.id=p.end WHERE n.keynode=1 ORDER BY p.start,p.end")?;
    let mut rows = statement.query([])?;
    comma = false;
    while let Some(row) = rows.next()? {
        if comma {
            write!(out, ",")?;
        }
        comma = true;
        serde_json::to_writer(
            &mut *out,
            &json!({"start_change_id":row.get::<_,String>(0)?,"end_change_id":row.get::<_,String>(1)?,"intermediate_change_count":row.get::<_,u64>(2)?}),
        )?;
    }
    write!(out, "]}}")?;
    Ok(())
}

/// A bounded UTF-8 stream retains at most one incomplete code point across reads.
struct TextChunks {
    reader: Box<dyn Read>,
    tail: Vec<u8>,
    done: bool,
    id: String,
}
impl Iterator for TextChunks {
    type Item = agentlaw_search::Result<agentlaw_search::SearchDocument>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let result = (|| -> Work<String> {
            let mut bytes = std::mem::take(&mut self.tail);
            let mut buffer = [0u8; 32768];
            let n = self.reader.read(&mut buffer)?;
            bytes.extend_from_slice(&buffer[..n]);
            self.done = n == 0;
            let valid = match std::str::from_utf8(&bytes) {
                Ok(_) => bytes.len(),
                Err(e) if e.error_len().is_none() && !self.done => e.valid_up_to(),
                Err(_) => return Err("invalid UTF8".into()),
            };
            self.tail = bytes.split_off(valid);
            Ok(String::from_utf8(bytes)?)
        })();
        Some(
            result
                .map(|body| agentlaw_search::SearchDocument {
                    memory_id: self.id.clone(),
                    change_id: self.id.clone(),
                    body,
                    scope: "history".into(),
                })
                .map_err(|_| {
                    agentlaw_search::Error::Invalid("Cannot stream acquired history text.".into())
                }),
        )
    }
}
fn search(
    db: &Connection,
    cache: &Path,
    query: &str,
    limit: usize,
    control: &RequestControl,
) -> Work<Vec<String>> {
    let mut index = agentlaw_search::SearchIndex::open(cache)?;
    // This is an immutable source-position/identity cache. A new source position
    // cannot make old entries appear current; only history search uses this index.
    if index.read_view()?.stamp.ack == 0 {
        let mut statement = db.prepare("SELECT id FROM nodes ORDER BY ordinal")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            control.check()?;
            let id: String = row.get(0)?;
            let reader = component(db, &id, "body")?
                .open()?
                .chain(std::io::Cursor::new(b"\n".to_vec()))
                .chain(component(db, &id, "evidence")?.open()?);
            index.commit_stream(
                0,
                std::iter::once(Ok(id.clone())),
                TextChunks {
                    reader: Box::new(reader),
                    tail: Vec::new(),
                    done: false,
                    id,
                },
            )?;
        }
        index.commit(1, &[], &[])?;
    }
    let hits = index.read_view()?.search(
        query,
        &agentlaw_search::ScopeFilter {
            allowed_scopes: vec!["history".into()],
        },
        limit,
        &[],
        &[],
        true,
    )?;
    Ok(hits.into_iter().map(|h| h.change_id).collect())
}

fn packet(
    db: &Connection,
    request: &HistoryRequest,
    records: &Path,
    map: &Path,
    out: &mut impl Write,
    maximum: Option<u64>,
    diagnostic: Option<&Value>,
) -> Work<()> {
    let query = request.history_for.is_some();
    let key = if query {
        "match_windows"
    } else if request.start_change_id.is_some() {
        "changes"
    } else {
        "latest_changes"
    };
    write!(out, "{{\"{key}\":[")?;
    let mut statement =
        db.prepare("SELECT window,offset,bytes FROM rendered ORDER BY window,ordinal LIMIT ?1")?;
    let mut rows =
        statement.query([maximum.map(i64::try_from).transpose()?.unwrap_or(i64::MAX)])?;
    let mut input = File::open(records)?;
    let mut current = None;
    let mut comma = false;
    while let Some(row) = rows.next()? {
        let window: i64 = row.get(0)?;
        if query && current != Some(window) {
            if current.is_some() {
                write!(out, "]}},")?;
            }
            write!(out, "{{\"changes\":[")?;
            comma = false;
            current = Some(window);
        }
        if comma {
            write!(out, ",")?;
        }
        comma = true;
        let offset: u64 = row.get(1)?;
        let bytes: u64 = row.get(2)?;
        input.seek(SeekFrom::Start(offset))?;
        if std::io::copy(&mut (&mut input).take(bytes), out)? != bytes {
            return Err("truncated projection".into());
        }
    }
    if query && current.is_some() {
        write!(out, "]}}")?;
    }
    write!(out, "],\"history_map\":")?;
    std::io::copy(&mut File::open(map)?, out)?;
    if let Some(diagnostic) = diagnostic {
        write!(out, ",\"diagnostics\":[")?;
        serde_json::to_writer(&mut *out, diagnostic)?;
        write!(out, "]")?;
    }
    write!(out, "}}")?;
    Ok(())
}
/// Answers one history request for the store owner from a single source acquisition.
pub fn history(
    ctx: &crate::runtime::ReadCtx<'_>,
    request: &HistoryRequest,
) -> crate::Result<Value> {
    let identity = request
        .memory_id
        .as_deref()
        .or(request.procedure_id.as_deref())
        .ok_or_else(|| {
            DomainError::new("invalid_history_request", "Specify one history identity.")
        })?;
    crate::validate_id(identity)?;
    ctx.request.check()?;
    ctx.request.phase("acquiring_history");
    // OwnedPublishedReader releases the shared source gate before returning.
    let acquired = ctx.source.acquire_history(&ctx.request.cancel);
    ctx.request.check()?;
    let acquired = crate::runtime::source(acquired)?;
    ctx.request.check()?;
    let mut spool = crate::runtime::source(acquired.into_spool())?;
    ctx.request.check()?;
    let workspace = ProjectionWorkspace::for_spool(
        &ctx.identity.local,
        &mut spool,
        identity,
        request,
        ctx.request,
    );
    ctx.request.check()?;
    let workspace = workspace.map_err(|_| failure())?;
    let semantic = PortHistorySemantic { ctx };
    project_spool(
        spool,
        &ctx.identity.local,
        request,
        ctx.identity.history_limit.min(ctx.identity.response_limit),
        ctx.request,
        &semantic,
        Some((ctx, &workspace)),
        None,
    )
}
fn search_owned(
    db: &Connection,
    ctx: &crate::runtime::ReadCtx<'_>,
    spool: &HistorySpool,
    identity: &str,
    query: &str,
    limit: usize,
    spill: bool,
) -> Work<Vec<agentlaw_search::Hit>> {
    let key = serde_json::to_string(&(
        &spool.source_position.epoch,
        spool.source_position.sequence,
        identity,
    ))?;
    let entry = ctx.history_cache.lexical(&key, ctx.request, || {
        let result = (|| -> Work<crate::history_cache::LexicalCache> {
            let directory = if spill {
                let root = ctx.identity.local.join("canonical").join("history-spools");
                std::fs::create_dir_all(&root)?;
                Some(tempfile::tempdir_in(root)?)
            } else {
                None
            };
            let path = directory
                .as_ref()
                .map(|d| d.path().join("lexical.sqlite"))
                .unwrap_or_else(|| {
                    std::path::PathBuf::from(format!(
                        "file:history-lexical-{}?mode=memory&cache=shared",
                        uuid::Uuid::new_v4()
                    ))
                });
            let mut index = agentlaw_search::SearchIndex::open_temporary(path)?;
            let mut st = db.prepare("SELECT id FROM nodes ORDER BY ordinal")?;
            let mut rows = st.query([])?;
            while let Some(row) = rows.next()? {
                ctx.request.check()?;
                let id: String = row.get(0)?;
                let reader = component(db, &id, "body")?
                    .open()?
                    .chain(std::io::Cursor::new(b"\n".to_vec()))
                    .chain(component(db, &id, "evidence")?.open()?);
                index.commit_stream(
                    0,
                    std::iter::once(Ok(id.clone())),
                    TextChunks {
                        reader: Box::new(reader),
                        tail: Vec::new(),
                        done: false,
                        id,
                    },
                )?;
            }
            index.commit(1, &[], &[])?;
            Ok(crate::history_cache::LexicalCache { index, directory })
        })();
        ctx.request.check()?;
        result.map_err(|_| failure())
    })?;
    let guard = loop {
        ctx.request.check()?;
        match entry.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(25))
            }
        }
    };
    let _keep_directory = &guard.directory;
    let hits = guard.index.read_view()?.search(
        query,
        &agentlaw_search::ScopeFilter {
            allowed_scopes: vec!["history".into()],
        },
        limit,
        &[],
        &[],
        true,
    )?;
    Ok(hits)
}

fn project_spool(
    mut spool: HistorySpool,
    local: &Path,
    request: &HistoryRequest,
    limit: usize,
    control: &RequestControl,
    semantic: &dyn HistorySemanticChannel,
    owned: Option<(&crate::runtime::ReadCtx<'_>, &ProjectionWorkspace)>,
    config_file: Option<&Path>,
) -> crate::Result<Value> {
    let identity = request
        .memory_id
        .as_deref()
        .or(request.procedure_id.as_deref())
        .ok_or_else(|| {
            DomainError::new("invalid_history_request", "Specify one history identity.")
        })?;
    let legacy_directory = if owned.is_none() {
        std::fs::create_dir_all(local).map_err(|_| failure())?;
        Some(tempfile::tempdir_in(local).map_err(|_| failure())?)
    } else {
        None
    };
    let legacy_workspace = if let Some(directory) = legacy_directory {
        let db =
            Connection::open(directory.path().join("projection.sqlite")).map_err(|_| failure())?;
        db.execute_batch("PRAGMA temp_store=FILE; PRAGMA cache_size=-2048; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;").map_err(|_| failure())?;
        Some(ProjectionWorkspace {
            db,
            directory: Some(directory),
        })
    } else {
        None
    };
    let workspace = owned
        .map(|(_, workspace)| workspace)
        .or(legacy_workspace.as_ref())
        .expect("workspace");
    let ctx = owned.map(|(ctx, _)| ctx);
    let notice_context = NoticeContext {
        config_file: ctx
            .and_then(|ctx| ctx.identity.config_file.as_deref())
            .or(config_file),
        general: ctx.is_some_and(|ctx| ctx.identity.response_limit <= ctx.identity.history_limit),
    };
    let db = &workspace.db;
    let work = (|| -> Work<Value> {
        db.execute_batch("CREATE TABLE nodes(ordinal INTEGER PRIMARY KEY,id TEXT UNIQUE,metadata TEXT,body TEXT,evidence TEXT,parents INTEGER,children INTEGER DEFAULT 0,keynode INTEGER DEFAULT 0); CREATE TABLE edges(child TEXT,parent TEXT,PRIMARY KEY(child,parent)); CREATE INDEX edge_parent ON edges(parent,child); CREATE TABLE selected(window INTEGER,id TEXT,matched INTEGER,PRIMARY KEY(window,id)); BEGIN;")?;
        let mut ordinal = 0u64;
        let count = spool.visit_metadata_closure(identity, |change| {
            let result = (|| -> Work<()> {
                control.check()?;
                let id = change.metadata["change_id"].as_str().ok_or("missing identity")?;
                let parents = change.metadata["parent_change_ids"].as_array().ok_or("missing parents")?;
                db.execute("INSERT INTO nodes(ordinal,id,metadata,body,evidence,parents) VALUES(?1,?2,?3,?4,?5,?6)", params![ordinal,id,serde_json::to_string(&change.metadata)?,serde_json::to_string(&change.body)?,serde_json::to_string(&change.evidence)?,parents.len() as u64])?;
                for parent in parents { db.execute("INSERT INTO edges VALUES(?1,?2)", params![id,parent.as_str().ok_or("invalid parent")?])?; }
                ordinal = ordinal.checked_add(1).ok_or("history count overflow")?;
                Ok(())
            })();
            result.map_err(|e| agentlaw_storage::Error::Io(std::io::Error::other(e.to_string())))
        })?;
        db.execute_batch("COMMIT; UPDATE nodes SET children=(SELECT count(*) FROM edges e WHERE e.parent=nodes.id); UPDATE nodes SET keynode=1 WHERE parents<>1 OR children<>1;")?;
        if count == 0 {
            return Ok(
                json!({"code":"history_not_found","message":"No history exists for this identity."}),
            );
        }
        let mut diagnostic = None;
        if let (Some(start), Some(end)) = (&request.start_change_id, &request.end_change_id) {
            for id in [start, end] {
                if db
                    .query_row("SELECT 1 FROM nodes WHERE id=?1", [id], |r| {
                        r.get::<_, i64>(0)
                    })
                    .optional()?
                    .is_none()
                {
                    return Ok(
                        json!({"code":"history_boundary_not_found","message":"Use change IDs from this history map."}),
                    );
                }
            }
            db.execute("WITH RECURSIVE descendants(id) AS (SELECT ?1 UNION SELECT e.child FROM edges e JOIN descendants d ON e.parent=d.id), ancestors(id) AS (SELECT ?2 UNION SELECT e.parent FROM edges e JOIN ancestors a ON e.child=a.id) INSERT INTO selected SELECT 0,id,0 FROM descendants INTERSECT SELECT 0,id,0 FROM ancestors",params![start,end])?;
            if db.query_row("SELECT count(*) FROM selected", [], |r| r.get::<_, u64>(0))? == 0 {
                return Ok(
                    json!({"code":"invalid_history_interval","message":"The end must be causally reachable from the start."}),
                );
            }
        } else if let Some(query) = &request.history_for {
            let maximum = request.max_matches.unwrap_or(1) as usize;
            if maximum > 10000 {
                return Ok(
                    json!({"code":"invalid_history_request","message":"max_matches exceeds the supported request-safety limit of 10000; no value was silently clamped."}),
                );
            }
            let matches = if let Some(ctx) = ctx {
                let lexical = search_owned(
                    db,
                    ctx,
                    &spool,
                    identity,
                    query,
                    maximum,
                    workspace.directory.is_some(),
                )?;
                // A newly indexed change can rank ahead of the acquired DAG.
                // Fetch the bounded candidate set before filtering and limiting.
                let candidates = semantic.candidates(identity, query, 10_000, control);
                control.check()?;
                let mut channels = vec![candidates_in_history(db, lexical)?];
                match candidates {
                    Ok(result) => {
                        if result.coverage
                            != crate::semantic::SemanticCoverage::CompleteAt(
                                spool.source_position.clone(),
                            )
                        {
                            diagnostic = Some(
                                json!({"code":"semantic_channel_incomplete","message":"Historical semantic search is unavailable or incomplete; a missing result is not evidence that no relevant history exists."}),
                            );
                        }
                        let same_epoch = match &result.coverage {
                            crate::semantic::SemanticCoverage::CompleteAt(position) => {
                                position.epoch == spool.source_position.epoch
                            }
                            crate::semantic::SemanticCoverage::Partial {
                                available,
                                requested,
                            } => {
                                available.epoch == spool.source_position.epoch
                                    && requested.epoch == spool.source_position.epoch
                            }
                            crate::semantic::SemanticCoverage::Unavailable(_) => false,
                        };
                        if same_epoch {
                            channels.push(candidates_in_history(db, result.hits)?);
                        }
                    }
                    Err(_) => {
                        diagnostic = Some(
                            json!({"code":"semantic_channel_incomplete","message":"Historical semantic search is unavailable or incomplete; a missing result is not evidence that no relevant history exists."}),
                        )
                    }
                }
                agentlaw_search::reciprocal_rank_fusion(&channels)
                    .into_iter()
                    .take(maximum)
                    .map(|hit| hit.change_id)
                    .collect::<Vec<_>>()
            } else {
                let directory = local.join("history-search");
                std::fs::create_dir_all(&directory)?;
                use sha2::{Digest, Sha256};
                let key = format!(
                    "{:x}",
                    Sha256::digest(format!(
                        "{}:{}:{identity}",
                        spool.source_position.epoch, spool.source_position.sequence
                    ))
                );
                control.phase("searching_history");
                let candidates =
                    semantic.search(db, &directory, identity, &spool, query, maximum, control);
                control.check()?;
                match candidates {
                    Ok(ids) => ids,
                    Err(_) => {
                        diagnostic = Some(
                            json!({"code":"semantic_channel_incomplete","message":"Historical semantic search is unavailable or incomplete. These results use the shared BM25 lexical backend only; a missing result is not evidence that no relevant history exists. Retry when the embedding worker is ready."}),
                        );
                        search(
                            db,
                            &directory.join(format!("{key}.sqlite")),
                            query,
                            maximum,
                            control,
                        )?
                    }
                }
            };
            for (rank, id) in matches.into_iter().enumerate() {
                if ctx.is_none()
                    || db
                        .query_row("SELECT 1 FROM nodes WHERE id=?1", [&id], |r| {
                            r.get::<_, i64>(0)
                        })
                        .optional()?
                        .is_some()
                {
                    db.execute(
                        "INSERT INTO selected VALUES(?1,?2,1)",
                        params![rank as i64, id],
                    )?;
                    select_ancestors(db, rank as i64, request.context_layers.unwrap_or(0))?;
                }
            }
        } else {
            db.execute_batch("INSERT INTO selected SELECT 0,id,0 FROM nodes WHERE children=0;")?;
            select_ancestors(db, 0, request.context_layers.unwrap_or(0))?;
        }
        if let Some(ctx) = ctx {
            let offer = ctx
                .history_semantic
                .offer(crate::semantic::HistoryIngestHint {
                    binding: ctx.identity.binding_id.clone(),
                    subject: identity.to_owned(),
                    through: spool.source_position.clone(),
                });
            if request.history_for.is_some()
                && matches!(
                    offer,
                    crate::semantic::OfferResult::Busy | crate::semantic::OfferResult::Queued
                )
                && diagnostic.is_none()
            {
                diagnostic = Some(
                    json!({"code":"semantic_channel_incomplete","message":"Historical semantic search is not fully available; a missing result is not evidence that no relevant history exists."}),
                );
            }
        }
        if let Some(directory) = &workspace.directory {
            db.execute_batch("CREATE TABLE rendered(window INTEGER,ordinal INTEGER,id TEXT,offset INTEGER,bytes INTEGER);")?;
            return render_disk(
                db,
                directory.path(),
                local,
                request,
                limit,
                control,
                diagnostic.as_ref(),
                ctx.is_some(),
                Some(&notice_context),
            );
        }
        let mut entries = Vec::new();
        let mut st = db.prepare("SELECT s.window,s.id,s.matched FROM selected s JOIN nodes n ON n.id=s.id ORDER BY s.window,n.ordinal")?;
        let mut rows = st.query([])?;
        while let Some(row) = rows.next()? {
            control.check()?;
            let id: String = row.get(1)?;
            let mut bytes = Vec::new();
            write_change(
                db,
                &id,
                row.get::<_, i64>(2)? != 0,
                workspace.directory.as_ref().map(|d| d.path()),
                &mut bytes,
            )?;
            entries.push((
                row.get::<_, i64>(0)?,
                id,
                serde_json::from_slice::<Value>(&bytes)?,
            ));
        }
        let assemble = |entries: &[(i64, String, Value)]| -> Work<Value> {
            let mut map = Vec::new();
            write_map(db, &mut map)?;
            let map: Value = serde_json::from_slice(&map)?;
            let mut value = if request.history_for.is_some() {
                let mut windows: Vec<Value> = Vec::new();
                let mut last = None;
                for (window, _, change) in entries {
                    if last != Some(*window) {
                        windows.push(json!({"changes":[]}));
                        last = Some(*window);
                    }
                    windows.last_mut().unwrap()["changes"]
                        .as_array_mut()
                        .unwrap()
                        .push(change.clone());
                }
                json!({"match_windows":windows,"history_map":map})
            } else if request.start_change_id.is_some() {
                json!({"changes":entries.iter().map(|e| &e.2).collect::<Vec<_>>(),"history_map":map})
            } else {
                json!({"latest_changes":entries.iter().map(|e| &e.2).collect::<Vec<_>>(),"history_map":map})
            };
            if let Some(diagnostic) = &diagnostic {
                value["diagnostics"] = json!([diagnostic]);
            }
            Ok(value)
        };
        let complete = assemble(&entries)?;
        if serde_json::to_vec(&complete)?.len() <= limit {
            return Ok(complete);
        }
        let notice = history_notice(limit, Some(&notice_context));
        for delivered in (1..entries.len()).rev() {
            db.execute_batch(
                "UPDATE nodes SET keynode=CASE WHEN parents<>1 OR children<>1 THEN 1 ELSE 0 END;",
            )?;
            db.execute(
                "UPDATE nodes SET keynode=1 WHERE id=?1",
                [&entries[delivered - 1].1],
            )?;
            let mut partial = assemble(&entries[..delivered])?;
            partial["response_limit"] = notice.clone();
            partial["remaining_requested_changes"] = json!(entries.len() - delivered);
            partial["next_action"] = json!("The remaining requested changes are unread, not absent. Use history with the same identity and start_change_id/end_change_id from history_map to open the desired inclusive causal range. Adjust the indicated limit for a larger response.");
            if serde_json::to_vec(&partial)?.len() <= limit {
                return Ok(partial);
            }
        }
        let bytes = serde_json::to_vec(&complete)?;
        history_artifact(local, &mut bytes.as_slice(), notice)
    })();
    control.check()?;
    work.map_err(|_| failure())
}

pub fn project(
    store: &Store,
    local: &Path,
    request: &HistoryRequest,
    limit: usize,
    control: &RequestControl,
) -> crate::Result<Value> {
    project_with_worker(store, local, request, limit, control, None)
}
pub fn project_with_worker(
    store: &Store,
    local: &Path,
    request: &HistoryRequest,
    limit: usize,
    control: &RequestControl,
    worker: Option<(&agentlaw_worker::Client, &str)>,
) -> crate::Result<Value> {
    project_with_worker_config(store, local, request, limit, control, worker, None)
}
pub(crate) fn project_with_worker_config(
    store: &Store,
    local: &Path,
    request: &HistoryRequest,
    limit: usize,
    control: &RequestControl,
    worker: Option<(&agentlaw_worker::Client, &str)>,
    config_file: Option<&Path>,
) -> crate::Result<Value> {
    let identity = request
        .memory_id
        .as_deref()
        .or(request.procedure_id.as_deref())
        .ok_or_else(|| {
            DomainError::new("invalid_history_request", "Specify one history identity.")
        })?;
    crate::validate_id(identity)?;
    control.phase("acquiring_history");
    let acquired = store.spool_history();
    control.check()?;
    let spool = acquired.map_err(|_| failure())?;
    let semantic = WorkerHistorySemantic { worker };
    project_spool(
        spool,
        local,
        request,
        limit,
        control,
        &semantic,
        None,
        config_file,
    )
}

trait HistorySemanticChannel {
    fn candidates(
        &self,
        _identity: &str,
        _query: &str,
        _maximum: usize,
        _control: &RequestControl,
    ) -> std::result::Result<crate::semantic::SemanticCandidates, crate::semantic::SemanticError>
    {
        Err(crate::semantic::SemanticError::Unavailable(
            "history candidates unavailable".into(),
        ))
    }
    fn search(
        &self,
        db: &Connection,
        directory: &Path,
        identity: &str,
        spool: &HistorySpool,
        query: &str,
        maximum: usize,
        control: &RequestControl,
    ) -> Work<Vec<String>>;
}
struct PortHistorySemantic<'a, 'b> {
    ctx: &'a crate::runtime::ReadCtx<'b>,
}
impl HistorySemanticChannel for PortHistorySemantic<'_, '_> {
    fn candidates(
        &self,
        identity: &str,
        query: &str,
        maximum: usize,
        control: &RequestControl,
    ) -> std::result::Result<crate::semantic::SemanticCandidates, crate::semantic::SemanticError>
    {
        let vector = self.ctx.semantic.embed_query(query, control)?;
        self.ctx.history_semantic.search_available(
            &self.ctx.identity.binding_id,
            identity,
            &vector,
            &["history".into()],
            maximum,
            control,
        )
    }
    fn search(
        &self,
        _db: &Connection,
        _directory: &Path,
        identity: &str,
        _spool: &HistorySpool,
        query: &str,
        maximum: usize,
        control: &RequestControl,
    ) -> Work<Vec<String>> {
        self.candidates(identity, query, maximum, control)
            .map(|result| result.hits.into_iter().map(|hit| hit.change_id).collect())
            .map_err(|_| "semantic search unavailable".into())
    }
}
struct WorkerHistorySemantic<'a> {
    worker: Option<(&'a agentlaw_worker::Client, &'a str)>,
}
impl HistorySemanticChannel for WorkerHistorySemantic<'_> {
    fn search(
        &self,
        db: &Connection,
        directory: &Path,
        identity: &str,
        spool: &HistorySpool,
        query: &str,
        maximum: usize,
        control: &RequestControl,
    ) -> Work<Vec<String>> {
        let worker = self.worker;
        let (client, binding) = worker.ok_or("semantic worker unavailable")?;
        if client.availability()? != agentlaw_worker::SemanticAvailability::Ready {
            return Err("semantic worker not ready".into());
        }
        let mut index = crate::history_search::HistoricalSearch::open(
            client,
            directory.join(format!("{identity}-jobs.sqlite")),
            binding,
            identity,
            &spool.source_position.epoch,
        )?;
        let mut st = db.prepare("SELECT id FROM nodes ORDER BY ordinal")?;
        let mut rows = st.query([])?;
        while let Some(row) = rows.next()? {
            control.check()?;
            let id: String = row.get(0)?;
            if !index.contains(&id)? {
                let mut reader = component(&db, &id, "body")?
                    .open()?
                    .chain(std::io::Cursor::new(b"\n".to_vec()))
                    .chain(component(&db, &id, "evidence")?.open()?);
                index.ingest(&id, "history", &mut reader)?;
            }
        }
        let result = index.search(query, &["history".into()], maximum, &control.cancel)?;
        if !result.semantic_complete {
            return Err("historical semantic coverage incomplete".into());
        }
        Ok(
            agentlaw_search::reciprocal_rank_fusion(&[result.lexical, result.vector])
                .into_iter()
                .take(maximum)
                .map(|hit| hit.change_id)
                .collect(),
        )
    }
}
fn render_disk(
    db: &Connection,
    scratch: &Path,
    local: &Path,
    request: &HistoryRequest,
    limit: usize,
    control: &RequestControl,
    diagnostic: Option<&Value>,
    owned: bool,
    identity: Option<&NoticeContext<'_>>,
) -> Work<Value> {
    control.phase("projecting_history");
    let records = scratch.join("records.jsonl");
    let mut rendered = File::create(&records)?;
    let mut statement=db.prepare("SELECT s.window,s.id,s.matched,n.ordinal FROM selected s JOIN nodes n ON n.id=s.id ORDER BY s.window,n.ordinal")?;
    let mut rows = statement.query([])?;
    let mut total = 0u64;
    while let Some(row) = rows.next()? {
        control.check()?;
        let id: String = row.get(1)?;
        let start = rendered.stream_position()?;
        write_change(
            &db,
            &id,
            row.get::<_, i64>(2)? != 0,
            Some(scratch),
            &mut rendered,
        )?;
        let size = rendered
            .stream_position()?
            .checked_sub(start)
            .ok_or("offset underflow")?;
        db.execute(
            "INSERT INTO rendered VALUES(?1,?2,?3,?4,?5)",
            params![
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(3)?,
                id,
                start,
                size
            ],
        )?;
        total = total.checked_add(1).ok_or("count overflow")?;
    }
    rendered.flush()?;
    let map = scratch.join("map.json");
    write_map(&db, &mut File::create(&map)?)?;
    let all = scratch.join("complete.json");
    packet(
        &db,
        request,
        &records,
        &map,
        &mut File::create(&all)?,
        None,
        diagnostic,
    )?;
    let bytes = all.metadata()?.len();
    let fits = !owned || inline_assembly_fits(&mut File::open(&all)?, control)?;
    if bytes <= limit as u64 && fits {
        return Ok(serde_json::from_reader(File::open(&all)?)?);
    }
    let notice = history_notice(limit, identity);
    if owned && !fits {
        let mut notice = notice;
        if bytes <= limit as u64 {
            notice["user_notice"] = json!("History exceeds the in-memory response budget. The complete response is available in the file.");
            notice["in_memory_budget_bytes"] = json!(HISTORY_WORKSPACE_MEMORY_LIMIT);
        }
        return history_artifact(local, &mut File::open(&all)?, notice);
    }
    let overhead = serde_json::to_vec(&notice)?.len() + 768;
    let available = limit
        .saturating_sub(overhead)
        .saturating_sub(map.metadata()?.len() as usize);
    let mut delivered = 0u64;
    let mut used = 0usize;
    let mut statement = db.prepare("SELECT bytes FROM rendered ORDER BY window,ordinal")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let size = usize::try_from(row.get::<_, u64>(0)?)?
            .checked_add(32)
            .ok_or("size overflow")?;
        if used.checked_add(size).is_none_or(|n| n > available) {
            break;
        }
        used += size;
        delivered += 1;
    }
    while delivered > 0 {
        // Keep delivered boundary nodes in the compressed map, so the reader
        // can ask an inclusive range without walking one cursor per change.
        db.execute_batch(
            "UPDATE nodes SET keynode=CASE WHEN parents<>1 OR children<>1 THEN 1 ELSE 0 END;",
        )?;
        db.execute("UPDATE nodes SET keynode=1 WHERE id=(SELECT id FROM rendered ORDER BY window,ordinal LIMIT 1 OFFSET ?1)",[delivered-1])?;
        write_map(&db, &mut File::create(&map)?)?;
        let partial = scratch.join("partial.json");
        packet(
            &db,
            request,
            &records,
            &map,
            &mut File::create(&partial)?,
            Some(delivered),
            diagnostic,
        )?;
        if partial.metadata()?.len() + overhead as u64 <= limit as u64 {
            let mut value: Value = serde_json::from_reader(File::open(&partial)?)?;
            value["response_limit"] = notice.clone();
            value["remaining_requested_changes"] = json!(total - delivered);
            value["next_action"]=json!("The remaining requested changes are unread, not absent. Use history with the same identity and start_change_id/end_change_id from history_map to open the desired inclusive causal range. Adjust the indicated limit for a larger response.");
            if serde_json::to_vec(&value)?.len() <= limit {
                return Ok(value);
            }
        }
        delivered -= 1;
    }
    // One change/map cannot fit. Freeze the entire requested projection in a
    // derived file; never claim an unread artifact was delivered inline.
    history_artifact(local, &mut File::open(&all)?, notice)
}

#[cfg(test)]
mod tests;
