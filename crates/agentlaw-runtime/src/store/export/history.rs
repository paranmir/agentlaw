//! Explicit lossless audit artifact. Never sent as ordinary recall content.
use agentlaw_contracts::{DomainError, RequestControl, Result};
use agentlaw_storage::history_spool::Component;
use agentlaw_storage::published::OwnedPublishedReader;
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::Path,
};

fn error(message: &str) -> DomainError {
    DomainError::new("history_export_failed", message)
}
pub(super) fn export(
    store: &OwnedPublishedReader,
    memory_id: &str,
    output: &Path,
    control: &RequestControl,
) -> Result<Value> {
    control.phase("exporting_history");
    control.check()?;
    agentlaw_storage::validate_id(memory_id)
        .map_err(|_| DomainError::new("invalid_memory_id", "Use a returned UUID memory ID."))?;
    let target = super::validate_output(store, output)?;
    let absolute = output.to_path_buf();
    let directory = &target.parent;
    // All causal selection and descriptors remain on disk. Large evidence/body
    // frames are streamed; one long line does not force a correspondingly large allocation.
    let scratch = tempfile::tempdir_in(&directory)
        .map_err(|_| error("Cannot prepare audit scratch space."))?;
    let db = agentlaw_sqlite::open(scratch.path().join("descriptors.sqlite"))
        .map_err(|_| error("Cannot open audit scratch space."))?;
    db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE; CREATE TABLE bodies(id TEXT PRIMARY KEY,component TEXT); CREATE TABLE roots(id TEXT PRIMARY KEY); CREATE TABLE heads(id TEXT PRIMARY KEY);").map_err(|_|error("Cannot initialize audit scratch space."))?;
    let mut records = tempfile::NamedTempFile::new_in(&directory)
        .map_err(|_| error("Cannot prepare audit records."))?;
    let count = store
        .visit_history_closure(memory_id, |change| {
            let result =
                (|| -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
                    if control.check().is_err() {
                        return Err("cancelled".into());
                    }
                    let id = change.metadata["change_id"]
                        .as_str()
                        .ok_or("missing change identity")?;
                    let parents = change.metadata["parent_change_ids"]
                        .as_array()
                        .ok_or("missing parents")?;
                    let base = if parents.len() > 1 {
                        change.metadata["delta_base_id"].as_str()
                    } else {
                        parents.first().and_then(Value::as_str)
                    };
                    let files = tempfile::tempdir_in(scratch.path())?;
                    let before = files.path().join("before");
                    let after = files.path().join("after");
                    if let Some(base) = base {
                        let descriptor: String = db.query_row(
                            "SELECT component FROM bodies WHERE id=?1",
                            [base],
                            |r| r.get(0),
                        )?;
                        serde_json::from_str::<Component>(&descriptor)?.materialize(&before)?;
                    } else {
                        std::fs::File::create(&before)?;
                    }
                    change.body.materialize(&after)?;
                    let millis = change.metadata["metadata_after"]["recorded_at_ms"]
                        .as_i64()
                        .ok_or("invalid timestamp")?;
                    let recorded_at = time::OffsetDateTime::from_unix_timestamp_nanos(
                        i128::from(millis) * 1_000_000,
                    )?
                    .format(&time::format_description::well_known::Rfc3339)?;
                    write!(records, "{{\"record_type\":\"change\",\"change_id\":")?;
                    serde_json::to_writer(&mut records, &id)?;
                    write!(records, ",\"parent_change_ids\":")?;
                    serde_json::to_writer(&mut records, parents)?;
                    write!(records, ",\"recorded_at\":")?;
                    serde_json::to_writer(&mut records, &recorded_at)?;
                    write!(records, ",\"evidence\":")?;
                    agentlaw_flows::history_diff::json_string(
                        &mut change.evidence.open()?,
                        &mut records,
                    )?;
                    write!(records, ",\"markdown_diff\":\"")?;
                    agentlaw_flows::history_diff::write_diff(
                        &before,
                        &after,
                        &files.path().join("lines.sqlite"),
                        &mut agentlaw_flows::history_diff::JsonEscape(&mut records),
                    )?;
                    write!(records, "\"")?;
                    if parents.len() > 1 {
                        write!(records, ",\"delta_base_id\":")?;
                        serde_json::to_writer(&mut records, &base)?;
                    }
                    writeln!(records, "}}")?;
                    db.execute(
                        "INSERT INTO bodies VALUES(?1,?2)",
                        params![id, serde_json::to_string(&change.body)?],
                    )?;
                    if parents.is_empty() {
                        db.execute("INSERT INTO roots VALUES(?1)", [id])?;
                    }
                    for parent in parents {
                        db.execute(
                            "DELETE FROM heads WHERE id=?1",
                            [parent.as_str().ok_or("invalid parent")?],
                        )?;
                    }
                    db.execute("INSERT INTO heads VALUES(?1)", [id])?;
                    Ok(())
                })();
            result.map_err(|e| agentlaw_storage::Error::Io(std::io::Error::other(e.to_string())))
        })
        .map_err(|_| {
            control.check().err().unwrap_or_else(|| {
                error(
                    "History validation or audit streaming failed; no partial audit was installed.",
                )
            })
        })?;
    if count == 0 {
        return Err(DomainError::new(
            "history_not_found",
            "No changes exist for this memory identity.",
        ));
    }
    let exported_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| error("Could not format export time."))?;
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)
        .map_err(|_| error("Could not prepare audit output."))?;
    let assemble = (|| -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        write!(
            temporary,
            "{{\"record_type\":\"manifest\",\"schema_version\":1,\"memory_id\":"
        )?;
        serde_json::to_writer(&mut temporary, memory_id)?;
        write!(temporary, ",\"exported_at\":")?;
        serde_json::to_writer(&mut temporary, &exported_at)?;
        for (field, sql) in [
            ("root_change_ids", "SELECT id FROM roots ORDER BY id"),
            ("head_change_ids", "SELECT id FROM heads ORDER BY id"),
        ] {
            write!(temporary, ",\"{field}\":[")?;
            let mut query = db.prepare(sql)?;
            let mut rows = query.query([])?;
            let mut comma = false;
            while let Some(row) = rows.next()? {
                if control.check().is_err() {
                    return Err("cancelled".into());
                }
                if comma {
                    write!(temporary, ",")?;
                }
                comma = true;
                serde_json::to_writer(&mut temporary, &row.get::<_, String>(0)?)?;
            }
            write!(temporary, "]")?;
        }
        writeln!(temporary, ",\"change_count\":{count}}}")?;
        records.flush()?;
        let mut input = std::fs::File::open(records.path())?;
        let mut buffer = [0u8; 32768];
        loop {
            if control.check().is_err() {
                return Err("cancelled".into());
            }
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            temporary.write_all(&buffer[..n])?;
        }
        Ok(())
    })();
    assemble.map_err(|_| {
        control
            .check()
            .err()
            .unwrap_or_else(|| error("Cannot assemble the complete audit artifact."))
    })?;
    control.check()?;
    super::install_output(temporary, &target, control)?;
    Ok(json!({"output":absolute,"change_count":count,"complete":true,"schema_version":1}))
}
