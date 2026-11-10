//! Write review: pending batches, overlap checks and procedure authoring.
use crate::read::{procedure_output, Channels};
use crate::runtime::*;
use crate::*;
use crate::{context::*, write::*};
use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::collections::BTreeSet;
pub enum Prepared {
    Replay(Value),
    Remember(RememberPlan),
    Discard(DiscardPlan),
    Authoring(AuthoringPlan),
    Procedure(ProcedurePlan),
    Connect(ConnectPlan),
}
pub(crate) enum PendingExpectation {
    New,
    Version(String),
}
pub struct RememberPlan {
    pub(crate) retained: Retained,
    pub(crate) expectation: PendingExpectation,
}
pub struct DiscardPlan {
    pub(crate) reference: PendingBatchRef,
    pub(crate) reason: String,
}
pub struct AuthoringPlan {
    pub(crate) token: uuid::Uuid,
    pub(crate) basis: String,
    pub(crate) response: Value,
}
pub struct ProcedurePlan {
    pub(crate) token: String,
    pub(crate) operation_id: uuid::Uuid,
    pub(crate) generation: u64,
    pub(crate) mutation: Mutation,
    pub(crate) submission: String,
}
pub struct ConnectPlan {
    pub(crate) kind: ConnectPlanKind,
    pub(crate) follow_up: Option<crate::apply::FollowUp>,
}
pub(crate) enum ConnectPlanKind {
    Select { path: String, project_id: String },
    Create { path: String, saga: ProjectSaga },
    Discovered(ConnectResponse),
}
pub struct ProjectSaga {
    pub(crate) path: String,
    pub(crate) project_id: String,
    pub(crate) operation_id: uuid::Uuid,
    pub(crate) payload: Value,
    pub(crate) resume: bool,
}

pub(crate) fn requires_overlap(
    snapshot: &Snapshot,
    p: &MemoryProposal,
    scope: &Applicability,
) -> bool {
    p.operation == Operation::Create
        || p.parent_refs
            .iter()
            .flatten()
            .chain(p.consolidation_refs.iter().flatten())
            .any(|r| {
                snapshot
                    .states
                    .get(&r.memory_id)
                    .is_none_or(|s| s.heads.iter().any(|h| &h.applicability != scope))
            })
}
fn winner(hits: &[agentlaw_search::Hit], minimum: f64, margin: f64) -> Option<(&str, bool)> {
    let top = hits.first()?;
    if top.score < minimum || hits.get(1).is_some_and(|h| h.score == top.score) {
        return None;
    }
    Some((
        &top.memory_id,
        hits.get(1).is_none_or(|h| top.score - h.score >= margin),
    ))
}
pub(crate) fn admitted(channels: &Channels) -> Option<String> {
    if !channels.complete {
        return None;
    }
    let (a, am) = winner(&channels.lexical, 0.90, 0.05)?;
    let (b, bm) = winner(&channels.vector, 0.92, 0.03)?;
    (a == b && (am || bm)).then(|| a.to_owned())
}
pub(crate) fn query_records(body: &str) -> Vec<&str> {
    let mut starts = Vec::new();
    let mut offset = 0;
    let mut fence: Option<(char, usize)> = None;
    for line in body.split_inclusive('\n') {
        let s = line.trim_start();
        if let Some(c) = s.chars().next().filter(|c| *c == '`' || *c == '~') {
            let n = s.chars().take_while(|x| *x == c).count();
            if n >= 3 {
                match fence {
                    None => fence = Some((c, n)),
                    Some((old, count)) if old == c && n >= count => fence = None,
                    _ => {}
                }
                offset += line.len();
                continue;
            }
        }
        if fence.is_none() {
            let n = s.chars().take_while(|c| *c == '#').count();
            if (1..=6).contains(&n) && s.as_bytes().get(n) == Some(&b' ') {
                starts.push(offset)
            }
        }
        offset += line.len();
    }
    let mut result = vec![body];
    for (i, start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(body.len());
        let section = &body[*start..end];
        if section != body && !section.trim().is_empty() {
            result.push(section)
        }
    }
    result
}
impl Runtime {
    fn load_pending(
        &self,
        reference: &PendingBatchRef,
    ) -> Result<(Retained, String, Option<String>)> {
        crate::control::load_pending(&self.control, &self.binding_id, reference)
    }
    pub(crate) fn pending_response(retained: &Retained, status: &str) -> Value {
        json!({"status":status,"pending_batch_ref":retained.batch.reference,"proposal_map":retained.batch.proposals.iter().enumerate().map(|(i,p)|json!({"proposal_index":i,"proposal_id":p.proposal_id})).collect::<Vec<_>>(),"proposals":retained.batch.proposals,"issues":retained.issues,"resolution_ref":retained.resolution_ref,"diagnostics":retained.diagnostics})
    }
    pub(crate) fn remember(&mut self, r: RememberRequest) -> Result<Value> {
        if r.procedure.is_some() {
            return self.procedure(&r);
        }
        let mut old_version = None;
        let mut retained = if let Some(reference) = &r.pending_batch_ref {
            let (mut retained, status, result) = self.load_pending(reference)?;
            match r.pending_action.as_ref() {
                Some(PendingAction::Inspect) => {
                    return if status == "published" {
                        decode(result.as_deref().unwrap_or("null"))
                    } else {
                        Ok(Self::pending_response(&retained, &status))
                    }
                }
                Some(PendingAction::Discard) => {
                    if r.user_confirmed != Some(true)
                        || r.discard_reason.as_deref().unwrap_or("").is_empty()
                    {
                        return Err(DomainError::new(
                            "invalid_input",
                            "Explicit user confirmation and a discard reason are required.",
                        ));
                    }
                    if retained.batch.reference != *reference {
                        return Err(DomainError::new(
                            "stale_pending_ref",
                            "Inspect latest pending work before discarding.",
                        ));
                    }
                    if status != "pending" || retained.operation_id.is_some() {
                        return Err(DomainError::new(
                            "publication_in_progress",
                            "A decided or completed publication cannot be discarded as pending.",
                        ));
                    }
                    let changed=sql(self.control.execute("UPDATE pending SET status='discarded',discard_reason=?1 WHERE id=?2 AND version=?3 AND status='pending'",params![r.discard_reason,reference.pending_batch_id,reference.observed_version]))?;
                    if changed != 1 {
                        return Err(DomainError::new(
                            "stale_pending_ref",
                            "Pending state changed.",
                        ));
                    }
                    return Ok(json!({"status":"discarded","pending_batch_ref":reference}));
                }
                Some(PendingAction::Continue) => {}
                _ => {
                    return Err(DomainError::new(
                        "invalid_input",
                        "pending_action is required.",
                    ))
                }
            }
            if status == "published" {
                return decode(result.as_deref().unwrap_or("null"));
            }
            if status != "pending" {
                return Err(DomainError::new(
                    "pending_discarded",
                    "This pending batch was explicitly discarded.",
                ));
            }
            if retained.batch.reference != *reference {
                return Err(DomainError::new(
                    "stale_pending_ref",
                    "Inspect the latest retained proposals before continuing.",
                ));
            }
            old_version = Some(reference.observed_version.clone());
            if let Some(delta) = &r.memories {
                if retained.operation_id.is_some() {
                    return Err(DomainError::new(
                        "publication_in_progress",
                        "Resume the retained execution before editing its proposals.",
                    ));
                }
                retained.batch = retained
                    .batch
                    .apply_delta(&retained.context, reference, delta)?;
            }
            retained
        } else {
            let context = resolve_context(self, r.project_path.as_deref())?;
            let proposals = r
                .memories
                .ok_or_else(|| DomainError::new("invalid_input", "memories are required."))?;
            Retained {
                diagnostics: vec![],
                batch: PendingBatch::new(&context, proposals),
                context,
                issues: vec![],
                basis: String::new(),
                resolution_ref: None,
                review_refs: vec![],
                approved_review_refs: vec![],
                overlap_approved: false,
                operation_id: None,
                mutations: vec![],
                expected_generation: None,
                publication_basis: None,
            }
        };
        // Replaying the same internal operation reuses its exact persisted mutations.
        if retained.operation_id.is_some() {
            return self.publish_retained(retained);
        }
        let mut snapshot = self.write_material(&retained.context, &retained.batch.proposals)?;
        struct StructuralOnly;
        impl ReviewGate for StructuralOnly {
            fn review(&self, _: &RequestContext, _: &[MemoryProposal], _: &ReadSet) -> Result<()> {
                Ok(())
            }
        }
        let prepared = preflight(
            &snapshot,
            &StructuralOnly,
            &retained.context,
            &retained.batch.proposals,
        )?;
        for (proposal, scope) in prepared
            .proposals
            .iter()
            .zip(&prepared.resolved_applicability)
        {
            if crate::prepare::requires_overlap(&snapshot, proposal, scope) {
                snapshot
                    .publication_basis
                    .watched_scopes
                    .push(canonical_scope(scope)?);
            }
        }
        let basis = digest(&(
            snapshot.basis.fingerprint.clone(),
            &prepared.proposals,
            &prepared.resolved_applicability,
        ))?;
        let same_basis = retained.basis == basis;
        if r.resolution_ref.is_some() && r.resolution_ref != retained.resolution_ref {
            return Err(DomainError::new("invalid_resolution_ref","The resolution token does not match retained review. Inspect the pending batch for its current context."));
        }
        let overlap_confirmed = same_basis
            && (retained.overlap_approved
                || (retained.resolution_ref.is_some()
                    && r.resolution_ref == retained.resolution_ref));
        let acknowledged: BTreeSet<_> = {
            retained
                .approved_review_refs
                .iter()
                .cloned()
                .chain(
                    r.review_judgments
                        .unwrap_or_default()
                        .into_iter()
                        .map(|r| r.review_ref),
                )
                .collect()
        };
        if acknowledged
            .iter()
            .any(|token| !retained.review_refs.contains(token))
        {
            return Err(DomainError::new(
                "invalid_review_ref",
                "A judgment does not match a retained dependent review.",
            ));
        }
        let affected: BTreeSet<_> = prepared
            .proposals
            .iter()
            .flat_map(|p| {
                p.parent_refs
                    .iter()
                    .flatten()
                    .chain(p.consolidation_refs.iter().flatten())
                    .map(|r| r.memory_id.clone())
            })
            .collect();
        let dependent: Vec<_> = snapshot
            .states
            .values()
            .filter(|s| {
                !affected.contains(&s.resolved_id)
                    && s.heads
                        .iter()
                        .any(|h| h.required_memory_ids.iter().any(|id| affected.contains(id)))
            })
            .collect();
        let (overlaps, diagnostics) = self.overlap_candidates(&snapshot, &prepared, &affected)?;
        retained.diagnostics = diagnostics;
        let mut issues = Vec::new();
        if !overlaps.is_empty() && !overlap_confirmed {
            issues.push(json!({"code":"possible_overlap","path":"/resolution_ref","message":"Review these current memories and confirm they remain distinct, or replace the retained proposals with evolve/consolidate requests.","current_memories":overlaps.iter().map(external_current).collect::<Vec<_>>()}));
        }
        let mut review_refs = Vec::new();
        for dependent in dependent {
            let required: BTreeSet<_> = dependent
                .heads
                .iter()
                .flat_map(|h| h.required_memory_ids.iter())
                .collect();
            let relevant: Vec<_> = prepared
                .proposals
                .iter()
                .filter(|p| {
                    p.parent_refs
                        .iter()
                        .flatten()
                        .chain(p.consolidation_refs.iter().flatten())
                        .any(|r| required.contains(&r.memory_id))
                })
                .collect();
            let token = digest(&(&dependent.resolved_id, &dependent.heads, relevant))?;
            review_refs.push(token.clone());
            if !acknowledged.contains(&token) {
                issues.push(json!({"code":"required_dependent_review","path":"/review_judgments","review_ref":token,"current_memory":external_current(dependent),"message":"Review this dependent against the proposed change; submit its complete update or an unchanged judgment."}));
            }
        }
        retained.basis = basis;
        retained.issues = issues;
        retained.review_refs = review_refs;
        retained.approved_review_refs = acknowledged
            .into_iter()
            .filter(|token| retained.review_refs.contains(token))
            .collect();
        retained.overlap_approved = overlap_confirmed;
        if !same_basis {
            retained.resolution_ref =
                (!overlaps.is_empty()).then(|| uuid::Uuid::new_v4().to_string());
        }
        retained.batch.reference.observed_version = uuid::Uuid::new_v4().to_string();
        if retained.issues.is_empty() {
            retained.mutations = self.mutations(&prepared, &retained.batch.proposals, &snapshot)?;
            retained.operation_id = Some(uuid::Uuid::new_v4().to_string());
            retained.expected_generation = Some(snapshot.generation);
            retained.publication_basis = Some(snapshot.publication_basis.clone());
        }
        self.save_pending(&retained, old_version.as_deref())?;
        if !retained.issues.is_empty() {
            return Ok(Self::pending_response(&retained, "resolution_required"));
        }
        self.publish_retained(retained)
    }
    fn mutations(
        &self,
        p: &PreparedIntent,
        originals: &[MemoryProposal],
        snapshot: &Snapshot,
    ) -> Result<Vec<Mutation>> {
        let mut result = Vec::new();
        for (index, (proposal, a)) in p
            .proposals
            .iter()
            .zip(&p.resolved_applicability)
            .enumerate()
        {
            let refs: Vec<_> = proposal
                .parent_refs
                .iter()
                .flatten()
                .chain(proposal.consolidation_refs.iter().flatten())
                .collect();
            let id = refs
                .iter()
                .map(|r| r.memory_id.clone())
                .min()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let mut relations: Vec<_> = proposal
                .related_memory_ids
                .iter()
                .flatten()
                .map(|id| json!({"kind":"related","target_memory_id":id}))
                .chain(
                    proposal
                        .required_memory_ids
                        .iter()
                        .flatten()
                        .map(|id| json!({"kind":"required","target_memory_id":id})),
                )
                .collect();
            // Omitted relationship fields inherit the entire canonical relation,
            // including optional descriptions, not merely the exposed target ID.
            let original = &originals[index];
            for (kind, explicit) in [
                ("related", original.related_memory_ids.is_some()),
                ("required", original.required_memory_ids.is_some()),
            ] {
                if !explicit && !refs.is_empty() {
                    let mut parent_sets = Vec::new();
                    for reference in &refs {
                        let unit = snapshot
                            .units
                            .iter()
                            .find(|u| u.entity_id == reference.memory_id)
                            .ok_or_else(|| {
                                DomainError::new(
                                    "source_changed",
                                    "Parent identity disappeared from the prepared snapshot.",
                                )
                            })?;
                        for h in unit.heads() {
                            if source(agentlaw_storage::version(unit, h))?
                                == reference.observed_version
                            {
                                let mut set = h.metadata["relations"]
                                    .as_array()
                                    .ok_or_else(|| {
                                        DomainError::new(
                                            "source_corrupt",
                                            "Parent relations are missing.",
                                        )
                                    })?
                                    .iter()
                                    .filter(|r| r["kind"] == kind)
                                    .cloned()
                                    .collect::<Vec<_>>();
                                set.sort_by_key(Value::to_string);
                                parent_sets.push(set)
                            }
                        }
                    }
                    if let Some(first) = parent_sets.first() {
                        if parent_sets.iter().any(|set| set != first) {
                            return Err(DomainError::new("metadata_resolution_required",format!("Explicitly resolve {kind} relationships; canonical parent relation metadata differs.")));
                        }
                        relations.retain(|r| r["kind"] != kind);
                        relations.extend(first.iter().cloned());
                    }
                }
            }
            let mut metadata = json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":canonical_scope(a)?,"origin":{"machine_id":self.machine_id,"project_id":p.context.project_id,"user_id":self.user_id},"recorded_at_ms":now_ms(),"is_rule":proposal.is_rule.unwrap_or(false),"relations":relations,"work_targets":proposal.work_targets.clone().unwrap_or_default()});
            if let Some(v) = proposal.in_working_set {
                metadata["in_working_set"] = json!(v)
            }
            let change = metadata["change_id"].as_str().unwrap().to_owned();
            result.push(Mutation {
                unit: CurrentUnit {
                    entity_id: id.clone(),
                    entity_type: "memory".into(),
                    state: UnitState::Live {
                        heads: vec![Head {
                            metadata,
                            body: proposal.what_to_remember.clone(),
                        }],
                    },
                },
                expected_versions: refs
                    .iter()
                    .filter(|r| r.memory_id == id)
                    .map(|r| r.observed_version.clone())
                    .collect(),
                evidence: proposal.evidence.clone(),
            });
            if proposal.operation == Operation::Consolidate {
                let sources: BTreeSet<_> = refs
                    .iter()
                    .filter(|r| r.memory_id != id)
                    .map(|r| r.memory_id.clone())
                    .collect();
                for redirected in sources {
                    result.push(Mutation {
                        unit: CurrentUnit {
                            entity_id: redirected.clone(),
                            entity_type: "memory".into(),
                            state: UnitState::Redirect {
                                redirect_to: id.clone(),
                                consolidation_change_id: change.clone(),
                            },
                        },
                        expected_versions: refs
                            .iter()
                            .filter(|r| r.memory_id == redirected)
                            .map(|r| r.observed_version.clone())
                            .collect(),
                        evidence: proposal.evidence.clone(),
                    });
                }
            }
        }
        Ok(result)
    }
    fn procedure(&self, r: &RememberRequest) -> Result<Value> {
        let previous: Option<(String, String, Option<String>)> =
            if let Some(token) = &r.authoring_ref {
                crate::control::authoring_result(&self.control, &self.binding_id, token)?
            } else {
                None
            };
        if let Some((status, submission, Some(result))) = &previous {
            if status == "published"
                && decode::<RememberRequest>(submission).and_then(|stored| encode(&stored))?
                    == encode(r)?
            {
                return decode(result);
            }
        }
        let p = r.procedure.as_ref().unwrap();
        let context = resolve_context(self, r.project_path.as_deref())?;
        let procedure_ids = p
            .parent_refs
            .as_ref()
            .map(|refs| {
                refs.iter()
                    .map(|r| r.procedure_id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let snapshot = self.snapshot_for_recall(&p.evidence_memory_ids, &procedure_ids)?;
        let mut evidence = Vec::new();
        for id in &p.evidence_memory_ids {
            validate_id(id)?;
            evidence.push(snapshot.current(id)?.ok_or_else(|| {
                DomainError::new("memory_not_found", "Procedure evidence memory is absent.")
            })?)
        }
        let mut parents = Vec::new();
        let mut stale_parents = false;
        let procedure_id = match p.operation {
            Operation::Create => None,
            Operation::Evolve => {
                let refs = p.parent_refs.as_ref().ok_or_else(|| {
                    DomainError::new("invalid_input", "Procedure evolve requires parent_refs.")
                })?;
                let ids: BTreeSet<_> = refs.iter().map(|r| r.procedure_id.clone()).collect();
                if ids.len() != 1 {
                    return Err(DomainError::new(
                        "invalid_parent_identity",
                        "Procedure evolve requires exactly one identity.",
                    ));
                }
                let id = ids.into_iter().next().unwrap();
                validate_id(&id)?;
                let unit = snapshot
                    .units
                    .iter()
                    .find(|u| u.entity_id == id && u.entity_type == "learned_procedure")
                    .ok_or_else(|| {
                        DomainError::new("procedure_not_found", "Procedure parent is absent.")
                    })?;
                let actual: BTreeSet<_> = source(unit.references())?
                    .into_iter()
                    .map(|r| snapshot.streaming.actual_version(r.observed_version))
                    .collect();
                let submitted: BTreeSet<_> =
                    refs.iter().map(|r| r.observed_version.clone()).collect();
                if actual != submitted {
                    stale_parents = true;
                }
                parents.extend(unit.heads().iter().cloned());
                Some(id)
            }
            Operation::Consolidate => {
                return Err(DomainError::unsupported(
                    "Procedure consolidation is not an accepted operation.",
                ))
            }
        };
        let applies = if let Some(scope) = &p.applies_to {
            if scope.contains(&ScopeKind::Project) && context.project_id.is_none() {
                return Err(DomainError::new(
                    "project_connection_required",
                    "Project-scoped procedure requires a connected project.",
                ));
            }
            Some(Applicability {
                scope: scope.clone(),
                project_id: if scope.contains(&ScopeKind::Project) {
                    context.project_id.clone()
                } else {
                    None
                },
                machine_id: scope
                    .contains(&ScopeKind::Machine)
                    .then(|| context.machine_id.clone()),
            })
        } else if !parents.is_empty() {
            let first = scope_value(&parents[0].metadata["applicability"])?;
            if parents
                .iter()
                .any(|h| scope_value(&h.metadata["applicability"]).ok().as_ref() != Some(&first))
            {
                return Err(DomainError::new(
                    "metadata_resolution_required",
                    "Specify applies_to because procedure parent scopes differ.",
                ));
            }
            Some(first)
        } else {
            None
        };
        // Preparation may omit create scope, so bind evidence/parents independently
        // of the final chosen applicability while still validating final scope.
        let basis = digest(&(
            self.binding_id.clone(),
            &p.operation,
            &p.evidence_memory_ids,
            evidence
                .iter()
                .flat_map(|state| state.heads.iter().map(|head| &head.memory_ref))
                .collect::<Vec<_>>(),
            snapshot
                .units
                .iter()
                .filter(|unit| unit.entity_type == "learned_procedure")
                .map(|unit| {
                    source(unit.references()).map(|refs| {
                        refs.into_iter()
                            .map(|r| snapshot.streaming.actual_version(r.observed_version))
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            "builtin-procedure-authoring-v1",
        ))?;
        let valid = if let Some(token) = &r.authoring_ref {
            sql(self
                .control
                .query_row(
                    "SELECT basis FROM authoring WHERE token=?1 AND binding=?2 AND used=0",
                    params![token, self.binding_id],
                    |row| row.get::<_, String>(0),
                )
                .optional())?
            .is_some_and(|stored| stored == basis)
        } else {
            false
        };
        if p.instructions.is_none() || !valid || stale_parents {
            let token = uuid::Uuid::new_v4().to_string();
            sql(self.control.execute(
                "INSERT INTO authoring(token,binding,basis) VALUES(?1,?2,?3)",
                params![token, self.binding_id, basis],
            ))?;
            let current = snapshot
                .units
                .iter()
                .filter(|u| {
                    u.entity_type == "learned_procedure"
                        && Some(&u.entity_id) == procedure_id.as_ref()
                })
                .map(|u| procedure_output(u, true))
                .collect::<Result<Vec<_>>>()?;
            let query = evidence
                .iter()
                .flat_map(|state| state.heads.iter())
                .map(|head| snapshot.streaming.excerpt(&head.what_to_remember, 512))
                .collect::<Result<Vec<_>>>()?
                .join("\n");
            let nearby = self.search_procedures(&query, r.project_path.as_deref(), 5)?;
            let mut response = json!({"status":"authoring_required","authoring_ref":token,"builtin_instructions":"Write a complete reusable instruction-only procedure. State when to use it, preserve conditions and exceptions supported by the supplied memories, distinguish observed evidence from hypotheses, and avoid executable installation scripts. Review every parent head. Nearby entries are descriptors only: use exact procedure_ids recall to read their full instructions before relying on them.","evidence_memories":evidence.iter().map(external_current).collect::<Vec<_>>(),"current_procedures":current,"nearby_procedures":nearby["procedures"],"diagnostics":nearby["diagnostics"]});
            if let Some((status, submission, result)) = previous {
                response["retained_submission"] = decode::<Value>(&submission)?;
                response["previous_execution_status"] = json!(status);
                if let Some(result) = result {
                    response["previous_execution_result"] = decode::<Value>(&result)?;
                }
                response["turn_instruction"]=json!("Inspect the retained submission and current evidence. Revise or resubmit the complete procedure using this fresh authoring_ref; the previous authorization is not reused.");
            }
            return snapshot
                .streaming
                .finish(response, &self.local, &self.request_control);
        }
        let applicability = applies.ok_or_else(|| {
            DomainError::new("invalid_input", "Final create requires applies_to.")
        })?;
        let id = procedure_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let metadata = json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":canonical_scope(&applicability)?,"origin":{"machine_id":self.machine_id,"project_id":context.project_id,"user_id":self.user_id},"recorded_at_ms":now_ms(),"name":p.name,"use_when":p.use_when,"evidence_memory_ids":p.evidence_memory_ids});
        let mutation = Mutation {
            unit: CurrentUnit {
                entity_id: id,
                entity_type: "learned_procedure".into(),
                state: UnitState::Live {
                    heads: vec![Head {
                        metadata,
                        body: p.instructions.clone().unwrap(),
                    }],
                },
            },
            expected_versions: p
                .parent_refs
                .iter()
                .flatten()
                .map(|r| r.observed_version.clone())
                .collect(),
            evidence: p.evidence.clone().ok_or_else(|| {
                DomainError::new("invalid_input", "Procedure evidence is required.")
            })?,
        };
        // Consume context and retain the exact authorized execution atomically.
        // A crash before canonical decision can resume this operation; it cannot
        // erase the final submitted instructions merely by consuming the token.
        let operation = uuid::Uuid::new_v4().to_string();
        let tx = sql(self.control.unchecked_transaction())?;
        let n = sql(tx.execute(
            "UPDATE authoring SET used=1 WHERE token=?1 AND binding=?2 AND used=0",
            params![r.authoring_ref, self.binding_id],
        ))?;
        if n != 1 {
            return Err(DomainError::new(
                "stale_authoring_ref",
                "Authoring context was used concurrently; prepare again.",
            ));
        }
        sql(tx.execute("INSERT INTO authoring_executions(token,binding,operation,generation,mutation,submission,status) VALUES(?1,?2,?3,?4,?5,?6,'pending')",params![r.authoring_ref,self.binding_id,operation,snapshot.generation.to_string(),encode(&mutation)?,encode(r)?]))?;
        sql(tx.commit())?;
        crate::apply::publish_procedure_parts(
            &self.store,
            &self.control,
            &self.request_control,
            r.authoring_ref.as_deref().ok_or_else(|| {
                DomainError::new("stale_authoring_ref", "Authoring context is missing.")
            })?,
            &operation,
            snapshot.generation,
            mutation,
            &encode(r)?,
        )
    }
    fn overlap_candidates(
        &self,
        snapshot: &Snapshot,
        prepared: &PreparedIntent,
        affected: &BTreeSet<String>,
    ) -> Result<(Vec<CurrentState>, Vec<DomainError>)> {
        let index = self.exact_index()?;
        let mut chosen = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for (p, scope) in prepared
            .proposals
            .iter()
            .zip(&prepared.resolved_applicability)
        {
            if !requires_overlap(snapshot, p, scope) {
                continue;
            }
            for record in query_records(&p.what_to_remember) {
                let channels =
                    self.search_scopes_channels(&index, &[scope_token(scope)], record, 128)?;
                if let Some(id) = admitted(&channels) {
                    if !affected.contains(&id) {
                        chosen.insert(id);
                    }
                }
                for diagnostic in channels.diagnostics {
                    if !diagnostics.contains(&diagnostic) {
                        diagnostics.push(diagnostic)
                    }
                }
            }
            for state in snapshot.states.values() {
                if !affected.contains(&state.resolved_id)
                    && state.heads.iter().any(|h| {
                        &h.applicability == scope && h.what_to_remember == p.what_to_remember
                    })
                {
                    chosen.insert(state.resolved_id.clone());
                }
            }
        }
        let mut result = Vec::new();
        for id in chosen {
            if let Some(state) = snapshot.current(&id)? {
                result.push(state)
            } else {
                let selected = self.snapshot_selected(&[id.clone()], &[])?;
                if selected.generation != snapshot.generation {
                    return Err(DomainError::new(
                        "source_changed",
                        "Overlap evidence changed during review.",
                    ));
                }
                if let Some(state) = selected.current(&id)? {
                    result.push(state)
                }
            }
        }
        Ok((result, diagnostics))
    }
}
