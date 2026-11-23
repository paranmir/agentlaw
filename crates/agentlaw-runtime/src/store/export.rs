//! Administrative export operations.

use crate::protocol::InventoryFilter;
use agentlaw_contracts::{DomainError, RequestControl};
use std::path::{Path, PathBuf};
mod history;
mod inventory;

struct OutputTarget {
    parent: PathBuf,
    destination: PathBuf,
}
fn validate_output(
    source: &agentlaw_storage::published::OwnedPublishedReader,
    output: &Path,
) -> Result<OutputTarget, DomainError> {
    if !output.is_absolute() {
        return Err(DomainError::new(
            "invalid_output_path",
            "Output path must be absolute.",
        ));
    }
    let name = output
        .file_name()
        .ok_or_else(|| DomainError::new("invalid_output_path", "Output path must name a file."))?;
    let parent = output.parent().ok_or_else(|| {
        DomainError::new("invalid_output_path", "Output path must have a parent.")
    })?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|_| DomainError::new("invalid_output_path", "Output directory must exist."))?;
    let root = source
        .with_source_read(|root, _| Ok(root.to_path_buf()))
        .map_err(|_| DomainError::new("source_unavailable", "Cannot verify source path."))?;
    if parent.starts_with(root) {
        return Err(DomainError::new(
            "invalid_output_path",
            "Write output outside the canonical memory store.",
        ));
    }
    let destination = parent.join(name);
    if destination.exists() {
        return Err(DomainError::new(
            "output_exists",
            "Choose a new output path.",
        ));
    }
    Ok(OutputTarget {
        parent,
        destination,
    })
}
fn install_output(
    mut temporary: tempfile::NamedTempFile,
    target: &OutputTarget,
    control: &RequestControl,
) -> Result<(), DomainError> {
    use std::io::Write;
    temporary
        .flush()
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|_| DomainError::new("output_failed", "Cannot flush output."))?;
    control.check()?;
    temporary
        .persist_noclobber(&target.destination)
        .map_err(|e| {
            if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                DomainError::new("output_exists", "Choose a new output path.")
            } else {
                DomainError::new("output_failed", "Cannot install completed output.")
            }
        })?;
    Ok(())
}

pub fn history_export(
    source: &agentlaw_storage::published::OwnedPublishedReader,
    memory_id: &str,
    output: &Path,
    control: &RequestControl,
) -> Result<serde_json::Value, DomainError> {
    history::export(source, memory_id, output, control)
}
pub fn procedure_list(
    source: &agentlaw_storage::published::OwnedPublishedReader,
    local: &Path,
    filter: &InventoryFilter,
    output: &Path,
    control: &RequestControl,
) -> Result<serde_json::Value, DomainError> {
    inventory::list(
        source,
        local,
        &inventory::Options {
            output: Some(output.to_path_buf()),
            scope: filter.scope.clone(),
            project: filter.project.clone(),
            machine: filter.machine.clone(),
            table: filter.table,
        },
        control,
    )
}
pub(super) fn write_search(
    source: &agentlaw_storage::published::OwnedPublishedReader,
    filter: &InventoryFilter,
    output: &Path,
    result: serde_json::Value,
    control: &RequestControl,
) -> Result<serde_json::Value, DomainError> {
    use std::io::Write;
    control.phase("exporting_procedure_search");
    control.check()?;
    let target = validate_output(source, output)?;
    let rows = result["procedures"]
        .as_array()
        .ok_or_else(|| DomainError::new("search_failed", "Procedure descriptors are missing."))?;
    let mut temp = tempfile::NamedTempFile::new_in(&target.parent)
        .map_err(|_| DomainError::new("output_failed", "Cannot prepare search output."))?;
    if filter.table {
        writeln!(temp, "PROCEDURE ID\tNAME\tUSE WHEN\tAPPLICABILITY")
            .map_err(|_| DomainError::new("output_failed", "Cannot write search output."))?;
    }
    for row in rows {
        control.check()?;
        if filter.table {
            writeln!(
                temp,
                "{}\t{}\t{}\t{}",
                row["procedure_id"], row["name"], row["use_when"], row["applicability"]
            )
        } else {
            writeln!(temp, "{row}")
        }
        .map_err(|_| DomainError::new("output_failed", "Cannot write search output."))?;
    }
    install_output(temp, &target, control)?;
    Ok(
        serde_json::json!({"output":output,"matched":result["matched"],"shown":result["shown"],"diagnostics":result["diagnostics"],"complete":result["matched"] == result["shown"]}),
    )
}

#[cfg(test)]
mod tests;
