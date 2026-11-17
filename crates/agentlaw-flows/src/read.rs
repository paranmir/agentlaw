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
    recall_prepared_with_diagnostics, search_procedures_filtered, PreparedRecall,
};
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
