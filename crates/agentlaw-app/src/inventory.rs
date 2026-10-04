//! Complete procedure inventory; disk-backed sorting keeps bodies out of the output.
use agentlaw_contracts::{DomainError, Result};
use agentlaw_storage::{Store, UnitState};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Default)]
pub struct Options {
    pub output: Option<PathBuf>,
    pub scope: Option<String>,
    pub project: Option<String>,
    pub machine: Option<String>,
    pub table: bool,
}

fn failure() -> DomainError {
    DomainError::new(
        "inventory_failed",
        "Complete inventory could not be produced. No partial output file was installed.",
    )
}

pub fn list(store: &Store, local: &Path, options: &Options) -> Result<Option<Value>> {
    if let Some(output) = &options.output {
        let absolute = if output.is_absolute() {
            output.clone()
        } else {
            std::env::current_dir().map_err(|_| failure())?.join(output)
        };
        let parent = absolute.parent().ok_or_else(failure)?;
        let parent = fs::canonicalize(parent).map_err(|_| failure())?;
        let root = store
            .with_source_read(|root, _| Ok(root.to_path_buf()))
            .map_err(|_| failure())?;
        if parent.starts_with(&root) {
            return Err(DomainError::new(
                "invalid_output_path",
                "Write the derived inventory outside the canonical memory store.",
            ));
        }
    }
    fs::create_dir_all(local).map_err(|_| failure())?;
    let scratch = tempfile::tempdir_in(local).map_err(|_| failure())?;
    let db = Connection::open(scratch.path().join("inventory.sqlite")).map_err(|_| failure())?;
    db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE; PRAGMA cache_size=-2048; CREATE TABLE rows(id TEXT NOT NULL,descriptor TEXT NOT NULL,PRIMARY KEY(id,descriptor)); CREATE TABLE competing(id TEXT PRIMARY KEY);").map_err(|_| failure())?;
    // Disposable derived output, never a source of memory. A crash simply discards this scratch.
    let project = if let Some(selector) = &options.project {
        let projects = store.list_catalog().map_err(|_| failure())?;
        let matches: Vec<_> = projects
            .iter()
            .filter(|p| {
                p["project_id"].as_str() == Some(selector)
                    || p["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(selector))
                    || p["aliases"].as_array().is_some_and(|v| {
                        v.iter()
                            .any(|n| n.as_str().is_some_and(|s| s.eq_ignore_ascii_case(selector)))
                    })
            })
            .filter_map(|p| p["project_id"].as_str())
            .collect();
        match matches.as_slice() {
            [id] => Some((*id).to_owned()),
            [] => {
                return Err(DomainError::new(
                    "project_not_found",
                    "No project matches the inventory selector.",
                ))
            }
            _ => {
                return Err(DomainError::new(
                    "project_selection_required",
                    "Several projects match; use the selected explicit project ID.",
                ))
            }
        }
    } else {
        None
    };
    store.visit_current(|unit| {
        if unit.entity_type != "learned_procedure" { return Ok(()); }
        if let UnitState::Live { heads } = &unit.state {
            for head in heads {
                let a = &head.metadata["applicability"];
                if options.scope.as_deref().is_some_and(|s| a["scope"].as_str() != Some(s))
                    || project.as_deref().is_some_and(|p| a["project_id"].as_str() != Some(p))
                    || options.machine.as_deref().is_some_and(|m| a["machine_id"].as_str() != Some(m)) { continue; }
                if !head.metadata["name"].is_string() || !head.metadata["use_when"].is_string() { return Err(agentlaw_storage::Error::Corrupt("procedure inventory descriptor".into())); }
                let record = json!({"procedure_id":unit.entity_id,"name":head.metadata["name"],"use_when":head.metadata["use_when"],"applicability":a});
                let descriptor = serde_json::to_string(&record)?;
                db.execute("INSERT OR IGNORE INTO rows VALUES(?1,?2)", params![unit.entity_id,descriptor])?;
                if heads.len() > 1 { db.execute("INSERT OR IGNORE INTO competing VALUES(?1)", [&unit.entity_id])?; }
            }
        }
        Ok(())
    }).map_err(|_| failure())?;
    let count: i64 = db
        .query_row("SELECT count(DISTINCT id) FROM rows", [], |r| r.get(0))
        .map_err(|_| failure())?;
    let competing: i64 = db
        .query_row("SELECT count(*) FROM competing", [], |r| r.get(0))
        .map_err(|_| failure())?;
    if competing > 0 {
        eprintln!("{competing} procedure IDs have competing current heads. All distinct descriptors are included; recall their IDs before choosing instructions.");
    }
    let write_rows = |out: &mut dyn Write| -> Result<()> {
        let mut statement = db
            .prepare(
                "SELECT descriptor FROM rows ORDER BY id COLLATE BINARY, descriptor COLLATE BINARY",
            )
            .map_err(|_| failure())?;
        let rows = statement
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|_| failure())?;
        if options.table {
            writeln!(out, "PROCEDURE ID\tNAME\tUSE WHEN\tAPPLICABILITY").map_err(|_| failure())?;
        }
        for row in rows {
            let row = row.map_err(|_| failure())?;
            if options.table {
                let v: Value = serde_json::from_str(&row).map_err(|_| failure())?;
                // JSON-quoted cells cannot inject tabs/newlines into the table layout.
                writeln!(
                    out,
                    "{}\t{}\t{}\t{}",
                    v["procedure_id"], v["name"], v["use_when"], v["applicability"]
                )
                .map_err(|_| failure())?;
            } else {
                writeln!(out, "{row}").map_err(|_| failure())?;
            }
        }
        out.flush().map_err(|_| failure())
    };
    if let Some(output) = &options.output {
        let output = if output.is_absolute() {
            output.clone()
        } else {
            std::env::current_dir().map_err(|_| failure())?.join(output)
        };
        let parent = output.parent().ok_or_else(failure)?;
        if output.exists() {
            return Err(DomainError::new(
                "output_exists",
                "Choose a new inventory output path; existing files are not overwritten.",
            ));
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|_| failure())?;
        write_rows(temp.as_file_mut())?;
        temp.as_file().sync_all().map_err(|_| failure())?;
        temp.persist_noclobber(&output).map_err(|_| failure())?;
        Ok(Some(
            json!({"output":output,"procedure_count":count,"complete":true}),
        ))
    } else {
        write_rows(&mut io::stdout().lock())?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentlaw_storage::{CurrentUnit, Head, Mutation};
    #[test]
    fn complete_inventory_is_sorted_compact_and_no_clobber() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("source"), temp.path().join("control")).unwrap();
        let ids = [
            "ffffffff-ffff-4fff-8fff-ffffffffffff",
            "00000000-0000-4000-8000-000000000001",
        ];
        for id in ids {
            store.publish(&uuid::Uuid::new_v4().to_string(),vec![Mutation {unit:CurrentUnit {
                entity_type:"learned_procedure".into(), entity_id:id.into(),state:UnitState::Live { heads:vec![Head {
                    metadata:json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"name":"Shell safety","use_when":"Before PowerShell file operations","evidence_memory_ids":[]}),
                    body:"Instructions must not be in inventory output.".into(),
                }]}
            },expected_versions:vec![],evidence:"test".into()}]).unwrap();
        }
        let output = temp.path().join("inventory.jsonl");
        let options = Options {
            output: Some(output.clone()),
            ..Options::default()
        };
        assert_eq!(
            list(&store, temp.path(), &options).unwrap().unwrap()["procedure_count"],
            2
        );
        let text = fs::read_to_string(&output).unwrap();
        let rows: Vec<Value> = text
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(rows[0]["procedure_id"], ids[1]);
        assert_eq!(rows[0].as_object().unwrap().len(), 4);
        assert!(!text.contains("Instructions must"));
        assert_eq!(
            list(&store, temp.path(), &options).unwrap_err().code,
            "output_exists"
        );
        assert_eq!(fs::read_to_string(&output).unwrap(), text);
        let bad = Options {
            output: Some(temp.path().join("source/new.md")),
            ..Options::default()
        };
        assert_eq!(
            list(&store, temp.path(), &bad).unwrap_err().code,
            "invalid_output_path"
        );
        assert!(!temp.path().join("source/new.md").exists());
    }
}
