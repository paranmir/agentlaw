//! Source-exact reads following disk-backed membership and discovery selection.
use crate::store::*;
use crate::*;
use crate::{context::*, recall::*, write::*};
use agentlaw_storage::{CurrentUnit, Head, UnitState};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
mod owner;
pub(crate) mod streaming;
pub(crate) use owner::preparation_channels;
pub use owner::{
    discover, prepare_recall, recall, recall_for_project, recall_prepared,
    recall_prepared_with_diagnostics, recall_with_snapshot, search_procedures_filtered,
    PreparedRecall,
};
/// Classifies failures from reading persisted source data for recall.
/// Request validation and derived-index failures have separate boundaries.
pub fn recall_source_error(error: agentlaw_storage::Error) -> DomainError {
    use agentlaw_storage::Error;
    use std::io::ErrorKind;

    fn read_failed(kind: ErrorKind) -> DomainError {
        source_operation_failed(format!(
            "Could not open or read the stored source ({kind:?})."
        ))
    }

    match error {
        Error::LocalRecallSpool(_) => recall_spool_error(),
        Error::LocalRecallEpoch(_) => recall_epoch_error(),
        Error::Corrupt(_) => corrupt_recall_source(
            "Stored source failed an integrity check for length, format, or checksum.",
        ),
        Error::Io(error) if error.kind() == ErrorKind::UnexpectedEof => corrupt_recall_source(
            "Stored source failed an integrity check because a frame ended before its declared length.",
        ),
        Error::Io(error) => read_failed(error.kind()),
        Error::Json(error) => match error.io_error_kind() {
            Some(kind) => read_failed(kind),
            None => corrupt_recall_source("Stored source metadata failed validation as JSON."),
        },
        Error::Capacity | Error::InsufficientResource { .. } => {
            recall_capacity("Source acquisition exceeded its capacity.")
        }
        Error::RecoveryRequired(_) => DomainError::new(
            "recovery_required",
            "Source publication recovery must complete before access.",
        ),
        Error::CoverageLost => DomainError::new(
            "source_coverage_lost",
            "Source publication coverage is incomplete.",
        ),
        Error::Stale(_) => DomainError::new(
            "stale_memory_ref",
            "Source state changed; inspect current heads and retry review.",
        ),
        Error::NotFound(_) => DomainError::new("memory_not_found", "Requested source identity is absent."),
        _ => source_operation_failed("Could not complete the stored source acquisition."),
    }
}

fn corrupt_recall_source(message: &str) -> DomainError {
    DomainError::new("source_corrupt", message).with_next_action(
        "Preserve the source. Run doctor --audit, restore the source from a verified copy, and repeat the recall after recovery.",
    )
}

fn source_operation_failed(message: impl Into<String>) -> DomainError {
    DomainError::new("source_operation_failed", message).with_next_action(
        "Check source read permissions, the mount, and storage access. Resolve the access or read failure before repeating the recall.",
    )
}

pub(crate) fn recall_capacity(message: &str) -> DomainError {
    let mut error = DomainError::new("capacity", message).with_next_action(
        "If unindexed changes exceeded the query budget, repeat the recall after the index catches up. For other acquisition limits, adjust the request scope or available capacity before repeating the recall.",
    );
    error.retryable = true;
    error
}

fn recall_spool_error() -> DomainError {
    let mut error = DomainError::new("recall_spool_unavailable", "The local recall spool could not be created, read, or verified.").with_next_action(
        "Discard the temporary spool and repeat the original recall to acquire a new copy. If this repeats, check local temporary directory access, storage, and cleanup jobs.",
    );
    error.retryable = true;
    error
}

fn recall_epoch_error() -> DomainError {
    let mut error = DomainError::new("recall_epoch_unavailable", "The local source-epoch record could not be read or verified.").with_next_action(
        "Check access to and integrity of the local state directory and epoch file for this binding. Repeat the original recall once the local state is available and valid.",
    );
    error.retryable = true;
    error
}

#[derive(Debug)]
enum RecallFailure {
    Recoverable(DomainError),
    Cancelled(DomainError),
    DeliveryFailed(DomainError),
}

type RecallResult<T> = std::result::Result<T, RecallFailure>;

impl From<DomainError> for RecallFailure {
    fn from(error: DomainError) -> Self {
        Self::Recoverable(error)
    }
}

impl From<RecallFailure> for DomainError {
    fn from(failure: RecallFailure) -> Self {
        match failure {
            RecallFailure::Recoverable(error)
            | RecallFailure::Cancelled(error)
            | RecallFailure::DeliveryFailed(error) => error,
        }
    }
}

fn check_recall(control: &RequestControl) -> RecallResult<()> {
    control.check().map_err(RecallFailure::Cancelled)
}

/// Inspect retained pending work without opening a writable control connection.
pub fn pending_inspect(ctx: &ReadCtx<'_>, request: &RememberRequest) -> Result<Value> {
    ctx.request.check()?;
    let reference = request
        .pending_batch_ref
        .as_ref()
        .ok_or_else(|| DomainError::new("invalid_input", "pending_batch_ref is required."))?;
    ctx.control
        .pending_inspect(&ctx.identity.binding_id, reference)
}
pub(crate) struct Channels {
    pub(crate) hits: Vec<agentlaw_search::Hit>,
    pub(crate) lexical: Vec<agentlaw_search::Hit>,
    pub(crate) vector: Vec<agentlaw_search::Hit>,
    pub(crate) complete: bool,
    pub(crate) diagnostics: Vec<DomainError>,
}
pub(crate) fn procedure_output(u: &CurrentUnit, full: bool) -> Result<Value> {
    let mut heads = Vec::new();
    for h in u.heads() {
        let mut value = json!({"procedure_id":u.entity_id,"name":h.metadata["name"],"use_when":h.metadata["use_when"],"applicability":scope_value(&h.metadata["applicability"])?});
        if full {
            value["instructions"] = json!(h.body);
            value["procedure_ref"] = json!({"procedure_id":u.entity_id,"observed_version":source(agentlaw_storage::version(u,h))?});
            value["evidence_memory_ids"] = h.metadata["evidence_memory_ids"].clone();
        }
        heads.push(value)
    }
    if heads.len() == 1 {
        Ok(heads.remove(0))
    } else {
        Ok(
            json!({"procedure_id":u.entity_id,"current_heads":heads,"head_reconciliation_required":true,"head_reconciliation_instruction":"Review all competing procedure heads; do not merge their instructions implicitly."}),
        )
    }
}

pub(crate) fn snapshot_selected_read(
    source_reader: &agentlaw_storage::published::OwnedPublishedReader,
    request_control: &RequestControl,
    ids: &[String],
    procedures: &[String],
) -> Result<Snapshot> {
    request_control.check()?;
    let guard = source(source_reader.read_snapshot())?;
    let (position, resolved, missing) = source(guard.read_closure(ids))?;
    let mut states = BTreeMap::new();
    let mut units = BTreeMap::new();
    let mut refs = Vec::new();
    for value in resolved {
        let unit = value.current;
        let memories = unit
            .heads()
            .iter()
            .map(|h| memory_from(&unit, h))
            .collect::<Result<Vec<_>>>()?;
        refs.extend(memories.iter().map(|m| m.memory_ref.clone()));
        let state = CurrentState {
            requested_id: value.requested_id.clone(),
            resolved_id: unit.entity_id.clone(),
            heads: memories,
            redirect_path: value.redirect_path,
        };
        states.insert(
            unit.entity_id.clone(),
            CurrentState {
                requested_id: unit.entity_id.clone(),
                redirect_path: vec![],
                ..state.clone()
            },
        );
        states.insert(value.requested_id, state);
        units.insert(unit.entity_id.clone(), unit);
    }
    let mut owned_bytes: usize = units
        .values()
        .flat_map(|u| u.heads())
        .map(|h| h.body.len())
        .sum();
    for id in procedures {
        validate_id(id)?;
        match guard.read_procedure(id) {
            Ok(unit) => {
                owned_bytes = owned_bytes
                    .saturating_add(unit.heads().iter().map(|h| h.body.len()).sum::<usize>());
                if owned_bytes > 64 * 1024 * 1024 {
                    return Err(DomainError::new(
                        "source_payload_requires_spool",
                        "Selected source requires file-backed delivery.",
                    ));
                }
                units.insert(id.clone(), unit);
            }
            Err(agentlaw_storage::Error::NotFound(_)) => {}
            Err(e) => return source(Err(e)),
        }
    }
    if *guard.position() != position {
        return Err(DomainError::new(
            "source_changed",
            "Source changed during exact reads; retry against a fresh read view.",
        ));
    }
    let units: Vec<_> = units.into_values().collect();
    refs.sort_by(|a, b| {
        (&a.memory_id, &a.observed_version).cmp(&(&b.memory_id, &b.observed_version))
    });
    refs.dedup();
    Ok(Snapshot {
        streaming: Default::default(),
        generation: position.sequence,
        publication_basis: agentlaw_storage::ReadSet {
            source_position: position,
            observed: units
                .iter()
                .filter(|u| u.entity_type == "memory")
                .map(|u| {
                    Ok((
                        u.entity_id.clone(),
                        source(u.references())?
                            .into_iter()
                            .map(|r| r.observed_version)
                            .collect(),
                    ))
                })
                .collect::<Result<_>>()?,
            absent: missing,
            reverse_required: BTreeMap::new(),
            watched_scopes: vec![],
        },
        states,
        basis: ReadSet {
            memory_refs: refs,
            fingerprint: digest(&units)?,
        },
        units,
    })
}
