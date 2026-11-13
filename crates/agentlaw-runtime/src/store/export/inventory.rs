//! Complete procedure inventory; disk-backed sorting keeps bodies out of the output.
use agentlaw_contracts::{DomainError, RequestControl, Result};
use agentlaw_storage::{published::OwnedPublishedReader, UnitState};
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

pub(super) fn list(
    store: &OwnedPublishedReader,
    local: &Path,
    options: &Options,
    control: &RequestControl,
) -> Result<Value> {
    control.phase("exporting_inventory");
    control.check()?;
    let output = options
        .output
        .as_ref()
        .ok_or_else(|| DomainError::new("invalid_output_path", "Output path is required."))?;
    let target = super::validate_output(store, output)?;
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
        if control.check().is_err() { return Err(agentlaw_storage::Error::Io(io::Error::other("cancelled"))); }
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
    }).map_err(|_| control.check().err().unwrap_or_else(failure))?;
    let count: i64 = db
        .query_row("SELECT count(DISTINCT id) FROM rows", [], |r| r.get(0))
        .map_err(|_| failure())?;
    let competing: i64 = db
        .query_row("SELECT count(*) FROM competing", [], |r| r.get(0))
        .map_err(|_| failure())?;
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
            control.check()?;
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
        let output = output.clone();
        let mut temp = tempfile::NamedTempFile::new_in(&target.parent).map_err(|_| failure())?;
        write_rows(temp.as_file_mut())?;
        super::install_output(temp, &target, control)?;
        let competing_notice = (competing > 0).then(|| format!("{competing} procedure IDs have competing current heads. All distinct descriptors are included; recall their IDs before choosing instructions."));
        Ok(
            json!({"output":output,"procedure_count":count,"complete":true,"competing_count":competing,"competing_notice":competing_notice}),
        )
    } else {
        Err(DomainError::new(
            "invalid_output_path",
            "Output path is required.",
        ))
    }
}
