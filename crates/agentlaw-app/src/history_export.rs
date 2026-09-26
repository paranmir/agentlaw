//! Explicit lossless audit artifact. Never sent as ordinary recall content.
use agentlaw_contracts::{DomainError, Result};
use agentlaw_storage::history_spool::Component;
use agentlaw_storage::Store;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{io::Write, path::Path};

fn error(message: &str) -> DomainError {
    DomainError::new("history_export_failed", message)
}
pub fn export(store: &Store, memory_id: &str, output: &Path) -> Result<Value> {
    agentlaw_storage::validate_id(memory_id)
        .map_err(|_| DomainError::new("invalid_memory_id", "Use a returned UUID memory ID."))?;
    let absolute = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| error("Could not resolve the output path."))?
            .join(output)
    };
    let directory = absolute
        .parent()
        .ok_or_else(|| error("Output must have a parent directory."))?;
    let directory = std::fs::canonicalize(directory)
        .map_err(|_| error("Output directory does not exist or cannot be inspected."))?;
    let root = store
        .with_source_read(|root, _| Ok(root.to_path_buf()))
        .map_err(|_| error("Source view is unavailable; no audit was written."))?;
    if directory.starts_with(&root) {
        return Err(error(
            "Write the derived audit outside the canonical memory store.",
        ));
    }
    if absolute.exists() {
        return Err(DomainError::new(
            "output_exists",
            "Audit export does not overwrite an existing file. Choose a new output path.",
        ));
    }
    // All causal selection and descriptors remain on disk. Large evidence/body
    // frames are streamed; one long line does not force a correspondingly large allocation.
    let scratch = tempfile::tempdir_in(&directory)
        .map_err(|_| error("Cannot prepare audit scratch space."))?;
    let db = Connection::open(scratch.path().join("descriptors.sqlite"))
        .map_err(|_| error("Cannot open audit scratch space."))?;
    db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE; CREATE TABLE bodies(id TEXT PRIMARY KEY,component TEXT); CREATE TABLE roots(id TEXT PRIMARY KEY); CREATE TABLE heads(id TEXT PRIMARY KEY);").map_err(|_|error("Cannot initialize audit scratch space."))?;
    let mut records = tempfile::NamedTempFile::new_in(&directory)
        .map_err(|_| error("Cannot prepare audit records."))?;
    let count = store
        .visit_history_closure(memory_id, |change| {
            let result =
                (|| -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
                    crate::stream_diff::json_string(&mut change.evidence.open()?, &mut records)?;
                    write!(records, ",\"markdown_diff\":\"")?;
                    crate::stream_diff::write_diff(
                        &before,
                        &after,
                        &files.path().join("lines.sqlite"),
                        &mut crate::stream_diff::JsonEscape(&mut records),
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
            error("History validation or audit streaming failed; no partial audit was installed.")
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
        std::io::copy(&mut input, &mut temporary)?;
        Ok(())
    })();
    assemble.map_err(|_| error("Cannot assemble the complete audit artifact."))?;
    temporary
        .flush()
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|_| error("Could not persist the audit file."))?;
    temporary
        .persist_noclobber(&absolute)
        .map_err(|_| error("Could not install completed output; existing files were preserved."))?;
    Ok(json!({"output":absolute,"change_count":count,"complete":true,"schema_version":1}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
    #[test]
    fn export_has_exact_manifest_and_lossless_changes_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        let store = Store::open(&root, dir.path().join("local")).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut expected = vec![];
        for body in ["첫 기억\r\n끝", "첫 기억\r\n고친 끝"] {
            let unit = CurrentUnit {
                entity_id: id.clone(),
                entity_type: "memory".into(),
                state: UnitState::Live {
                    heads: vec![Head {
                        metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                        body: body.into(),
                    }],
                },
            };
            let receipt = store
                .publish(
                    &uuid::Uuid::new_v4().to_string(),
                    vec![Mutation {
                        unit,
                        expected_versions: expected,
                        evidence: "Observed correction".into(),
                    }],
                )
                .unwrap();
            expected = receipt
                .references
                .into_iter()
                .map(|r| r.observed_version)
                .collect();
        }
        let output = dir.path().join("audit.jsonl");
        assert_eq!(export(&store, &id, &output).unwrap()["change_count"], 2);
        let raw = std::fs::read_to_string(&output).unwrap();
        let records: Vec<Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records[0].as_object().unwrap().len(), 7);
        assert_eq!(records[0]["record_type"], "manifest");
        assert_eq!(records[1]["record_type"], "change");
        assert!(records[1].get("delta_base_id").is_none());
        assert_eq!(records[2]["parent_change_ids"][0], records[1]["change_id"]);
        assert!(records[2]["markdown_diff"]
            .as_str()
            .unwrap()
            .contains("고친 끝"));
        assert_eq!(
            export(&store, &id, &output).unwrap_err().code,
            "output_exists"
        );
        assert_eq!(std::fs::read_to_string(&output).unwrap(), raw);
        assert!(export(&store, &id, &root.join("audit.jsonl")).is_err());
    }
}
