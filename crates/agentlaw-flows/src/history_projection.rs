//! Disk-backed, causal history selection. Bodies are acquired from immutable C6
//! components; history length is not a bound on process memory.
use crate::{history_diff, DomainError, HistoryRequest, RequestControl};
use agentlaw_storage::{history_spool::Component, Store};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
type Work<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn failure() -> DomainError {
    DomainError::new("history_projection_failed", "History could not be acquired or projected completely. Original memory is unchanged; use doctor to investigate source integrity and retry. This is not an empty history result.")
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
    dir: &Path,
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
    let scratch = tempfile::tempdir_in(dir)?;
    let before = scratch.path().join("before");
    let after = scratch.path().join("after");
    if let Some(base) = base {
        component(db, base, "body")?.materialize(&before)?;
    } else {
        File::create(&before)?;
    }
    component(db, id, "body")?.materialize(&after)?;
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
    history_diff::write_diff(
        &before,
        &after,
        &scratch.path().join("lines.sqlite"),
        &mut history_diff::JsonEscape(out),
    )?;
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
    let identity = request
        .memory_id
        .as_deref()
        .or(request.procedure_id.as_deref())
        .ok_or_else(|| {
            DomainError::new("invalid_history_request", "Specify one history identity.")
        })?;
    crate::validate_id(identity)?;
    control.phase("acquiring_history");
    let work = (|| -> Work<Value> {
        std::fs::create_dir_all(local)?;
        let scratch = tempfile::tempdir_in(local)?;
        let db = Connection::open(scratch.path().join("projection.sqlite"))?;
        db.execute_batch("PRAGMA temp_store=FILE; PRAGMA cache_size=-2048; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; CREATE TABLE nodes(ordinal INTEGER PRIMARY KEY,id TEXT UNIQUE,metadata TEXT,body TEXT,evidence TEXT,parents INTEGER,children INTEGER DEFAULT 0,keynode INTEGER DEFAULT 0); CREATE TABLE edges(child TEXT,parent TEXT,PRIMARY KEY(child,parent)); CREATE INDEX edge_parent ON edges(parent,child); CREATE TABLE selected(window INTEGER,id TEXT,matched INTEGER,PRIMARY KEY(window,id)); CREATE TABLE rendered(window INTEGER,ordinal INTEGER,id TEXT,offset INTEGER,bytes INTEGER); BEGIN;")?;
        let mut spool = store.spool_history()?;
        let mut ordinal = 0u64;
        let count=spool.visit_metadata_closure(identity,|change|{
            let result=(||->Work<()>{control.check()?;let id=change.metadata["change_id"].as_str().ok_or("missing identity")?;
                let parents=change.metadata["parent_change_ids"].as_array().ok_or("missing parents")?;
                db.execute("INSERT INTO nodes(ordinal,id,metadata,body,evidence,parents) VALUES(?1,?2,?3,?4,?5,?6)",params![ordinal,id,serde_json::to_string(&change.metadata)?,serde_json::to_string(&change.body)?,serde_json::to_string(&change.evidence)?,parents.len() as u64])?;
                for parent in parents {db.execute("INSERT INTO edges VALUES(?1,?2)",params![id,parent.as_str().ok_or("invalid parent")?])?;}
                ordinal=ordinal.checked_add(1).ok_or("history count overflow")?;Ok(())})();
            result.map_err(|e|agentlaw_storage::Error::Io(std::io::Error::other(e.to_string())))
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
            let semantic = (|| -> Work<Vec<String>> {
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
            })();
            control.check()?;
            let matches = match semantic {
                Ok(ids) => ids,
                Err(_) => {
                    diagnostic = Some(
                        json!({"code":"semantic_channel_incomplete","message":"Historical semantic search is unavailable or incomplete. These results use the shared BM25 lexical backend only; a missing result is not evidence that no relevant history exists. Retry when the embedding worker is ready."}),
                    );
                    search(
                        &db,
                        &directory.join(format!("{key}.sqlite")),
                        query,
                        maximum,
                        control,
                    )?
                }
            };
            for (rank, id) in matches.into_iter().enumerate() {
                db.execute(
                    "INSERT INTO selected VALUES(?1,?2,1)",
                    params![rank as i64, id],
                )?;
                select_ancestors(&db, rank as i64, request.context_layers.unwrap_or(0))?;
            }
        } else {
            db.execute_batch("INSERT INTO selected SELECT 0,id,0 FROM nodes WHERE children=0;")?;
            select_ancestors(&db, 0, request.context_layers.unwrap_or(0))?;
        }
        control.phase("projecting_history");
        let records = scratch.path().join("records.jsonl");
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
                scratch.path(),
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
        let map = scratch.path().join("map.json");
        write_map(&db, &mut File::create(&map)?)?;
        let all = scratch.path().join("complete.json");
        packet(
            &db,
            request,
            &records,
            &map,
            &mut File::create(&all)?,
            None,
            diagnostic.as_ref(),
        )?;
        if all.metadata()?.len() <= limit as u64 {
            return Ok(serde_json::from_reader(File::open(&all)?)?);
        }
        let notice = json!({"applied_bytes":limit,"config_key":"history.response_limit_bytes","config_file":"Use agentlaw config path to obtain the installation configuration file.","user_notice":"History details exceed the configured response size. Whole changes are shown with a causal map; this setting can be changed with agentlaw config set history.response_limit_bytes <bytes>."});
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
            let partial = scratch.path().join("partial.json");
            packet(
                &db,
                request,
                &records,
                &map,
                &mut File::create(&partial)?,
                Some(delivered),
                diagnostic.as_ref(),
            )?;
            if partial.metadata()?.len() + overhead as u64 <= limit as u64 {
                let mut value: Value = serde_json::from_reader(File::open(&partial)?)?;
                value["response_limit"] = notice.clone();
                value["remaining_requested_changes"] = json!(total - delivered);
                value["next_action"]=json!("The remaining requested changes are unread, not absent. Use history with the same identity and start_change_id/end_change_id from history_map to open the desired inclusive causal range. Increase history.response_limit_bytes for a larger response.");
                if serde_json::to_vec(&value)?.len() <= limit {
                    return Ok(value);
                }
            }
            delivered -= 1;
        }
        // One change/map cannot fit. Freeze the entire requested projection in a
        // derived file; never claim an unread artifact was delivered inline.
        let artifact = crate::artifacts::create_from_reader(
            local.join("response-artifacts"),
            &mut File::open(&all)?,
        )?;
        Ok(
            json!({"code":"complete_content_in_file","content_read":false,"complete":true,"artifact":artifact,"response_limit":notice,"next_action":"The complete version-frozen history response is in the artifact and has not been read. Use the harness file-reading tool or shell to read that path in bounded sections before relying on it. If that environment cannot access the path, tell the user what is unread and ask for file transfer or raise the configured history response limit; do not claim the history was restored. After the indicated retention period it may be cleaned automatically; a new history query reads the then-current source, not necessarily this version."}),
        )
    })();
    // Cancellation gets its own outcome instead of being disguised as corruption.
    control.check()?;
    work.map_err(|_| failure())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
    fn fixture(count: u32) -> (tempfile::TempDir, Store, String, Vec<String>) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("source"), temp.path().join("canonical")).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut versions = vec![];
        let mut changes = vec![];
        for n in 0..count {
            let change = uuid::Uuid::new_v4().to_string();
            changes.push(change.clone());
            let body=format!("# Current understanding\nThis is revision {n} of the cancellation policy.\n{}\n한국어 끝", "unchanged context ".repeat(20));
            let unit = CurrentUnit {
                entity_id: id.clone(),
                entity_type: "memory".into(),
                state: UnitState::Live {
                    heads: vec![Head {
                        body,
                        metadata: json!({"change_id":change,"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":n as i64,"is_rule":false,"relations":[],"work_targets":[]}),
                    }],
                },
            };
            versions = store
                .publish(
                    &uuid::Uuid::new_v4().to_string(),
                    vec![Mutation {
                        unit,
                        expected_versions: versions,
                        evidence: format!(
                            "Observed cancellation revision {n}; preserve the reason."
                        ),
                    }],
                )
                .unwrap()
                .references
                .into_iter()
                .map(|r| r.observed_version)
                .collect();
        }
        (temp, store, id, changes)
    }
    fn request(id: &str) -> HistoryRequest {
        HistoryRequest {
            memory_id: Some(id.into()),
            procedure_id: None,
            context_layers: Some(0),
            history_for: None,
            max_matches: None,
            start_change_id: None,
            end_change_id: None,
        }
    }
    #[test]
    fn latest_has_compact_causal_map_not_one_node_per_revision() {
        let (temp, store, id, changes) = fixture(12);
        let value = project(
            &store,
            temp.path(),
            &request(&id),
            8192,
            &RequestControl::default(),
        )
        .unwrap();
        assert_eq!(value["latest_changes"].as_array().unwrap().len(), 1);
        assert_eq!(value["latest_changes"][0]["change_id"], changes[11]);
        assert_eq!(value["history_map"]["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(
            value["history_map"]["paths"][0]["intermediate_change_count"],
            10
        );
        assert!(value.get("response_limit").is_none());
    }
    #[test]
    fn causal_range_is_inclusive_and_budget_provides_details_with_remaining_map() {
        let (temp, store, id, changes) = fixture(12);
        let mut r = request(&id);
        r.context_layers = None;
        r.start_change_id = Some(changes[1].clone());
        r.end_change_id = Some(changes[10].clone());
        let complete =
            project(&store, temp.path(), &r, 100_000, &RequestControl::default()).unwrap();
        assert_eq!(complete["changes"].as_array().unwrap().len(), 10);
        assert_eq!(complete["changes"][0]["change_id"], changes[1]);
        assert_eq!(complete["changes"][9]["change_id"], changes[10]);
        let partial = project(&store, temp.path(), &r, 4096, &RequestControl::default()).unwrap();
        assert!(partial["changes"].as_array().is_some(), "{partial}");
        assert!(partial["remaining_requested_changes"].as_u64().unwrap() > 0);
        assert!(serde_json::to_vec(&partial).unwrap().len() <= 4096);
        assert_eq!(
            partial["response_limit"]["config_key"],
            "history.response_limit_bytes"
        );
        r.start_change_id = Some(changes[10].clone());
        r.end_change_id = Some(changes[1].clone());
        assert_eq!(
            project(&store, temp.path(), &r, 8192, &RequestControl::default()).unwrap()["code"],
            "invalid_history_interval"
        );
    }
    #[test]
    fn indivisible_change_uses_whole_frozen_file_and_search_is_ranked() {
        let (temp, store, id, _) = fixture(4);
        let mut r = request(&id);
        let small = project(&store, temp.path(), &r, 128, &RequestControl::default()).unwrap();
        assert_eq!(small["content_read"], false);
        let original: Value = serde_json::from_reader(
            File::open(small["artifact"]["path"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert!(original["latest_changes"][0]["markdown_diff"]
            .as_str()
            .unwrap()
            .contains("한국어 끝"));
        r.history_for = Some("cancellation 2".into());
        r.max_matches = Some(1);
        r.context_layers = Some(1);
        let query = project(&store, temp.path(), &r, 8192, &RequestControl::default()).unwrap();
        let window = query["match_windows"][0]["changes"].as_array().unwrap();
        assert_eq!(window.len(), 2);
        assert_eq!(window[1]["matched"], true);
        assert!(window[1]["evidence"]
            .as_str()
            .unwrap()
            .contains("revision 2"));
        assert!(window[0].get("matched").is_none());
    }
    #[test]
    fn map_keeps_forks_and_all_integration_parents() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE nodes(ordinal INTEGER,id TEXT,parents INTEGER,children INTEGER,keynode INTEGER); CREATE TABLE edges(child TEXT,parent TEXT); INSERT INTO nodes VALUES(0,'a',0,2,1),(1,'b',1,1,0),(2,'c',1,1,0),(3,'d',2,0,1); INSERT INTO edges VALUES('b','a'),('c','a'),('d','b'),('d','c');").unwrap();
        let mut bytes = vec![];
        write_map(&db, &mut bytes).unwrap();
        let map: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(map["nodes"][0]["roles"], json!(["root", "fork"]));
        assert_eq!(map["nodes"][3]["roles"], json!(["head", "integration"]));
        assert_eq!(map["paths"].as_array().unwrap().len(), 4);
        // Parallel compressed paths need distinct endpoints to remain selectable:
        // fork children are anchors, otherwise b and c would become indistinguishable.
    }
}
