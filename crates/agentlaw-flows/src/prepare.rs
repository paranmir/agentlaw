//! Write review: pending batches, overlap checks and procedure authoring.
use crate::read::{procedure_output, Channels};
use crate::semantic::{PiecePlan, PieceVectorReuse, SemanticError};
use crate::store::*;
use crate::*;
use crate::{context::*, write::*};
use agentlaw_storage::{CurrentUnit, Head, Mutation, UnitState};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

trait ReviewSource {
    fn position(&self) -> Result<agentlaw_storage::published::SourcePosition>;
    fn heads(&self, ids: &[String]) -> Result<Vec<crate::exact::HeadRecord>>;
    fn identical_bodies(&self, body: &str) -> Result<Vec<String>>;
    fn related(&self, ids: &[String]) -> Result<Vec<(String, String)>>;
    fn reverse_required(&self, ids: &[String]) -> Result<Vec<String>>;
    fn select_proposal(&mut self, _index: usize, _body: &str) -> Result<()> {
        Ok(())
    }
    fn search(&mut self, scopes: &[String], query: &str) -> Result<Channels>;
    fn snapshot(&self, ids: &[String], procedures: &[String]) -> Result<Snapshot>;
}

struct ReadReview<'a, 'b, 'c> {
    ctx: &'a ReadCtx<'b>,
    exact: &'c mut crate::exact::ExactSnapshot,
    plans: &'c mut BTreeMap<usize, Option<PiecePlan>>,
    reuse: &'c PieceVectorReuse,
    selected: Option<(usize, usize)>,
}
impl ReviewSource for ReadReview<'_, '_, '_> {
    fn position(&self) -> Result<agentlaw_storage::published::SourcePosition> {
        Ok(self.exact.position().clone())
    }
    fn heads(&self, ids: &[String]) -> Result<Vec<crate::exact::HeadRecord>> {
        self.exact.heads(ids)
    }
    fn identical_bodies(&self, body: &str) -> Result<Vec<String>> {
        self.exact.identical_bodies(body)
    }
    fn related(&self, ids: &[String]) -> Result<Vec<(String, String)>> {
        self.exact.related(ids)
    }
    fn reverse_required(&self, ids: &[String]) -> Result<Vec<String>> {
        self.exact.reverse_required(ids)
    }
    fn select_proposal(&mut self, index: usize, body: &str) -> Result<()> {
        if let std::collections::btree_map::Entry::Vacant(entry) = self.plans.entry(index) {
            let plan = plan_records(self.ctx, &query_records(body))?;
            entry.insert(plan);
        }
        self.selected = Some((index, 0));
        Ok(())
    }

    fn search(&mut self, scopes: &[String], query: &str) -> Result<Channels> {
        let planned = self.selected.as_mut().and_then(|(proposal, record)| {
            let index = *record;
            *record += 1;
            self.plans
                .get(proposal)?
                .as_ref()
                .map(|plan| (plan, index, self.reuse))
        });
        crate::read::preparation_channels(self.ctx, self.exact, query, scopes, planned)
    }
    fn snapshot(&self, ids: &[String], procedures: &[String]) -> Result<Snapshot> {
        let snapshot = crate::read::snapshot_selected_read(
            self.ctx.source,
            self.ctx.request,
            ids,
            procedures,
        )?;
        if snapshot.publication_basis.source_position != *self.exact.position() {
            return Err(DomainError::new(
                "source_changed",
                "Source changed during preparation; retry.",
            ));
        }
        Ok(snapshot)
    }
}

trait Preparation {
    fn identity(&self) -> StoreIdentity;
    fn request(&self) -> &RequestControl;
    fn context(&self, path: Option<&str>) -> Result<RequestContext>;
    fn load(&self, reference: &PendingBatchRef) -> Result<(Retained, String, Option<String>)>;
    fn authoring_result(&self, token: &str) -> Result<Option<(String, String, Option<String>)>>;
    fn authoring_basis(&self, token: &str) -> Result<Option<String>>;
    fn material(
        &mut self,
        context: &RequestContext,
        proposals: &[MemoryProposal],
    ) -> Result<Snapshot>;
    fn overlaps(
        &mut self,
        snapshot: &Snapshot,
        prepared: &PreparedIntent,
        affected: &BTreeSet<String>,
    ) -> Result<(Vec<CurrentState>, Vec<DomainError>)>;
    fn snapshot(&self, ids: &[String], procedures: &[String]) -> Result<Snapshot>;
    fn nearby(&mut self, query: &str, path: Option<&str>) -> Result<Value>;
}

struct ReadPreparation<'a, 'b, 'c> {
    ctx: &'a ReadCtx<'b>,
    exact: &'c mut crate::exact::ExactSnapshot,
    plans: BTreeMap<usize, Option<PiecePlan>>,
    reuse: PieceVectorReuse,
}
impl Preparation for ReadPreparation<'_, '_, '_> {
    fn identity(&self) -> StoreIdentity {
        let id = self.ctx.identity;
        StoreIdentity {
            root: id.root.clone(),
            local: id.local.clone(),
            user_id: id.user_id.clone(),
            machine_id: id.machine_id.clone(),
            binding_id: id.binding_id.clone(),
            history_limit: id.history_limit,
            response_limit: id.response_limit,
            config_file: id.config_file.clone(),
        }
    }
    fn request(&self) -> &RequestControl {
        self.ctx.request
    }
    fn context(&self, path: Option<&str>) -> Result<RequestContext> {
        let project_id = if let Some(path) = path {
            absolute_project_path(path)?;
            Some(
                self.ctx
                    .control
                    .exact_association(&self.ctx.identity.binding_id, path)?
                    .ok_or_else(|| {
                        DomainError::new(
                            "project_connection_required",
                            "Discover and explicitly select or create a project connection.",
                        )
                    })?
                    .project_id,
            )
        } else {
            None
        };
        Ok(RequestContext {
            store_binding_id: self.ctx.identity.binding_id.clone(),
            user_id: self.ctx.identity.user_id.clone(),
            machine_id: self.ctx.identity.machine_id.clone(),
            project_id,
            connection_version: "1".into(),
        })
    }
    fn load(&self, reference: &PendingBatchRef) -> Result<(Retained, String, Option<String>)> {
        self.ctx
            .control
            .load_pending(&self.ctx.identity.binding_id, reference)
    }
    fn authoring_result(&self, token: &str) -> Result<Option<(String, String, Option<String>)>> {
        self.ctx
            .control
            .authoring_result(&self.ctx.identity.binding_id, token)
    }
    fn authoring_basis(&self, token: &str) -> Result<Option<String>> {
        self.ctx
            .control
            .authoring_basis(&self.ctx.identity.binding_id, token)
    }
    fn material(
        &mut self,
        context: &RequestContext,
        proposals: &[MemoryProposal],
    ) -> Result<Snapshot> {
        write_material(
            &mut ReadReview {
                ctx: self.ctx,
                exact: self.exact,
                plans: &mut self.plans,
                reuse: &self.reuse,
                selected: None,
            },
            context,
            proposals,
        )
    }
    fn overlaps(
        &mut self,
        snapshot: &Snapshot,
        prepared: &PreparedIntent,
        affected: &BTreeSet<String>,
    ) -> Result<(Vec<CurrentState>, Vec<DomainError>)> {
        overlap_candidates(
            &mut ReadReview {
                ctx: self.ctx,
                exact: self.exact,
                plans: &mut self.plans,
                reuse: &self.reuse,
                selected: None,
            },
            snapshot,
            prepared,
            affected,
        )
    }
    fn snapshot(&self, ids: &[String], procedures: &[String]) -> Result<Snapshot> {
        let snapshot = crate::read::snapshot_selected_read(
            self.ctx.source,
            self.ctx.request,
            ids,
            procedures,
        )?;
        if snapshot.publication_basis.source_position != *self.exact.position() {
            return Err(DomainError::new(
                "source_changed",
                "Source changed during preparation; retry.",
            ));
        }
        Ok(snapshot)
    }
    fn nearby(&mut self, query: &str, path: Option<&str>) -> Result<Value> {
        let context = self.context(path)?;
        let scopes: Vec<_> = crate::exact::allowed_scopes(&context)
            .into_iter()
            .map(|scope| format!("procedure:{scope}"))
            .collect();
        let plan = plan_records(self.ctx, &[query])?;
        let planned = plan.as_ref().map(|plan| (plan, 0, &self.reuse));
        let channels =
            crate::read::preparation_channels(self.ctx, self.exact, query, &scopes, planned)?;
        let ids: Vec<_> = channels
            .hits
            .iter()
            .filter_map(|hit| hit.memory_id.strip_prefix("procedure/").map(str::to_owned))
            .take(5)
            .collect();
        let mut procedures = Vec::new();
        for head in self.exact.heads(&ids)? {
            if head.entity_type == "learned_procedure" {
                let applicability = scope_value(&head.metadata["applicability"])?;
                if scopes.contains(&format!("procedure:{}", scope_token(&applicability))) {
                    procedures.push(json!({"procedure_id":head.entity_id,"name":head.metadata["name"],"use_when":head.metadata["use_when"],"applicability":applicability}));
                }
            }
        }
        Ok(json!({"procedures":procedures,"diagnostics":channels.diagnostics}))
    }
}

fn plan_records(ctx: &ReadCtx<'_>, records: &[&str]) -> Result<Option<PiecePlan>> {
    match ctx.semantic.plan_pieces(records, ctx.request) {
        Ok(plan) => Ok(Some(plan)),
        Err(SemanticError::Cancelled) => {
            ctx.request.check()?;
            Err(DomainError::new(
                "cancelled",
                "Piece planning was cancelled.",
            ))
        }
        Err(_) => Ok(None),
    }
}

fn write_material(
    review: &mut impl ReviewSource,
    context: &RequestContext,
    proposals: &[MemoryProposal],
) -> Result<Snapshot> {
    let position = review.position()?;
    let inherited_parents: Vec<_> = proposals
        .iter()
        .filter(|p| {
            p.operation == Operation::Evolve
                && p.applies_to.is_some()
                && p.what_to_remember.is_none()
        })
        .filter_map(|p| match p.parent_refs.as_deref() {
            Some([reference]) => Some(reference.memory_id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let parents = if inherited_parents.is_empty() {
        None
    } else {
        Some(review.snapshot(&inherited_parents, &[])?)
    };
    let mut ids = BTreeSet::new();
    let mut affected = BTreeSet::new();
    for (index, p) in proposals.iter().enumerate() {
        for reference in p
            .parent_refs
            .iter()
            .flatten()
            .chain(p.consolidation_refs.iter().flatten())
        {
            ids.insert(reference.memory_id.clone());
            affected.insert(reference.memory_id.clone());
        }
        ids.extend(p.related_memory_ids.iter().flatten().cloned());
        ids.extend(p.required_memory_ids.iter().flatten().cloned());
        let mut scope_context = context.clone();
        if p.applies_to.is_none() {
            if let Some(reference) = p.parent_refs.iter().flatten().next() {
                if let Some(head) = review.heads(&[reference.memory_id.clone()])?.first() {
                    let applicability = scope_value(&head.metadata["applicability"])?;
                    scope_context.project_id = applicability.project_id;
                    if let Some(machine) = applicability.machine_id {
                        scope_context.machine_id = machine;
                    }
                }
            }
        }
        if p.operation == Operation::Create || p.applies_to.is_some() {
            let body = if let Some(body) = p.what_to_remember.as_deref() {
                body
            } else {
                let Some([reference]) = p.parent_refs.as_deref() else {
                    continue;
                };
                let Some(state) = parents
                    .as_ref()
                    .and_then(|s| s.states.get(&reference.memory_id))
                else {
                    continue;
                };
                if state.resolved_id != reference.memory_id
                    || !matches!(state.heads.as_slice(), [parent] if &parent.memory_ref == reference)
                {
                    continue;
                }
                proposal_body(p, &state.heads)?
            };
            review.select_proposal(index, body)?;
            ids.extend(review.identical_bodies(body)?);
            for record in query_records(body) {
                ids.extend(
                    review
                        .search(&crate::exact::allowed_scopes(&scope_context), record)?
                        .hits
                        .into_iter()
                        .map(|hit| hit.memory_id),
                );
            }
        }
    }
    let affected: Vec<_> = affected.into_iter().collect();
    ids.extend(review.related(&affected)?.into_iter().map(|item| item.1));
    ids.extend(review.reverse_required(&affected)?);
    let mut snapshot = review.snapshot(&ids.into_iter().collect::<Vec<_>>(), &[])?;
    for id in affected {
        snapshot
            .publication_basis
            .reverse_required
            .insert(id.clone(), review.reverse_required(&[id])?);
    }
    if snapshot.publication_basis.source_position != position {
        return Err(DomainError::new(
            "source_changed",
            "Source changed during write candidate selection; repeat review.",
        ));
    }
    Ok(snapshot)
}

fn overlap_candidates(
    review: &mut impl ReviewSource,
    snapshot: &Snapshot,
    prepared: &PreparedIntent,
    affected: &BTreeSet<String>,
) -> Result<(Vec<CurrentState>, Vec<DomainError>)> {
    let mut chosen = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for (index, (p, scope)) in prepared
        .proposals
        .iter()
        .zip(&prepared.resolved_applicability)
        .enumerate()
    {
        if !requires_overlap(snapshot, p, scope) {
            continue;
        }
        let body = proposal_body(p, &[])?;
        review.select_proposal(index, body)?;
        for record in query_records(body) {
            let channels = review.search(&[scope_token(scope)], record)?;
            if let Some(id) = admitted(&channels) {
                if !affected.contains(&id) {
                    chosen.insert(id);
                }
            }
            for diagnostic in channels.diagnostics {
                if !diagnostics.contains(&diagnostic) {
                    diagnostics.push(diagnostic);
                }
            }
        }
        for state in snapshot.states.values() {
            if !affected.contains(&state.resolved_id)
                && state
                    .heads
                    .iter()
                    .any(|head| &head.applicability == scope && head.what_to_remember == body)
            {
                chosen.insert(state.resolved_id.clone());
            }
        }
    }
    let mut result = Vec::new();
    for id in chosen {
        if let Some(state) = snapshot.current(&id)? {
            result.push(state);
        } else {
            let selected = review.snapshot(&[id], &[])?;
            if selected.generation != snapshot.generation {
                return Err(DomainError::new(
                    "source_changed",
                    "Overlap evidence changed during review.",
                ));
            }
            result.extend(
                selected
                    .states
                    .into_values()
                    .filter(|state| state.requested_id == state.resolved_id),
            );
        }
    }
    Ok((result, diagnostics))
}
pub enum Prepared {
    WithPrefixes(Box<Prepared>, Vec<SubmittedPrefix>),
    Replay(Value),
    Remember(RememberPlan),
    Discard(DiscardPlan),
    Authoring(AuthoringPlan),
    Procedure(ProcedurePlan),
    Connect(ConnectPlan),
}

pub fn prepare(
    ctx: &ReadCtx<'_>,
    exact: &mut crate::exact::ExactSnapshot,
    mut request: RememberRequest,
) -> Result<Prepared> {
    ctx.request.check()?;
    let prefixes = {
        let guard = source(ctx.source.read_snapshot())?;
        if guard.position() != exact.position() {
            return Err(DomainError::new(
                "source_changed",
                "Source changed during preparation; retry.",
            ));
        }
        normalize_write_ids(&guard, &mut request)?
    };
    let prepared = prepare_remember(
        &mut ReadPreparation {
            ctx,
            exact,
            plans: BTreeMap::new(),
            reuse: PieceVectorReuse::default(),
        },
        request,
    )?;
    if prefixes.is_empty() {
        Ok(prepared)
    } else {
        Ok(Prepared::WithPrefixes(Box::new(prepared), prefixes))
    }
}

pub fn prepare_connect(ctx: &ReadCtx<'_>, request: ConnectRequest) -> Result<Prepared> {
    ctx.request.check()?;
    absolute_project_path(&request.project_path)?;
    if request
        .memory_store_path
        .as_ref()
        .is_some_and(|path| std::path::Path::new(path) != ctx.identity.root)
    {
        return Err(DomainError::new(
            "store_binding_transition_required",
            "Select a different store through installation configuration.",
        ));
    }
    let existing = ctx
        .control
        .exact_association(&ctx.identity.binding_id, &request.project_path)?;
    let follow_up = (request.restore_context == Some(true)).then(|| crate::apply::FollowUp {
        restore: true,
        recall_for: request.recall_for.clone(),
    });
    let kind = match request.intent.clone().unwrap_or_default() {
        ConnectIntent::Discover => {
            let mut discovery = request.clone();
            discovery.restore_context = None;
            let value = crate::read::discover(ctx, &discovery)?;
            ConnectPlanKind::Discovered(serde_json::from_value(value).map_err(|_| {
                DomainError::new("serialization_failed", "Cannot decode project discovery.")
            })?)
        }
        ConnectIntent::Connect => {
            let id = request
                .project_id
                .as_ref()
                .ok_or_else(|| DomainError::new("invalid_input", "project_id is required."))?;
            validate_id(id)?;
            match ctx.source.read_catalog(id) {
                Ok(_) => {}
                Err(agentlaw_storage::Error::NotFound(_)) => {
                    return Err(DomainError::new(
                        "project_not_found",
                        "The selected project does not exist.",
                    ))
                }
                Err(error) => return source(Err(error)),
            }
            ConnectPlanKind::Select {
                path: request.project_path.clone(),
                project_id: id.clone(),
            }
        }
        ConnectIntent::Create => {
            if let Some(connection) = existing {
                ConnectPlanKind::Discovered(ConnectResponse {
                    code: None,
                    project_connection: Some(connection),
                    candidates: vec![],
                    recall_result: None,
                })
            } else {
                let name = request.project_name.as_ref().ok_or_else(|| {
                    DomainError::new("invalid_input", "project_name is required.")
                })?;
                let project_id = uuid::Uuid::new_v4().to_string();
                let payload = json!({"project_id":project_id,"name":name,"description":request.project_description,"aliases":[],"repository_observations":[],"catalog_revision":uuid::Uuid::new_v4().to_string()});
                ConnectPlanKind::Create {
                    path: request.project_path.clone(),
                    saga: ProjectSaga {
                        path: request.project_path,
                        project_id,
                        operation_id: uuid::Uuid::new_v4(),
                        payload,
                        resume: false,
                    },
                }
            }
        }
    };
    let follow_up = match &kind {
        ConnectPlanKind::Discovered(response) if response.project_connection.is_none() => None,
        _ => follow_up,
    };
    Ok(Prepared::Connect(ConnectPlan { kind, follow_up }))
}
pub(crate) enum PendingExpectation {
    New,
    Version(String),
}
pub struct RememberPlan {
    pub(crate) retained: Retained,
    pub(crate) expectation: PendingExpectation,
    pub(crate) review_basis: Option<agentlaw_storage::ReadSet>,
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
pub(crate) fn pending_response(retained: &Retained, status: &str) -> Value {
    json!({"status":status,"pending_batch_ref":retained.batch.reference,"proposal_map":retained.batch.proposals.iter().enumerate().map(|(i,p)|json!({"proposal_index":i,"proposal_id":p.proposal_id})).collect::<Vec<_>>(),"proposals":retained.batch.proposals,"issues":retained.issues,"resolution_ref":retained.resolution_ref,"diagnostics":retained.diagnostics})
}

fn stored_relations(head: &Head, kind: &str) -> Result<Vec<Value>> {
    let mut relations = head.metadata["relations"]
        .as_array()
        .ok_or_else(|| DomainError::new("source_corrupt", "Parent relations are missing."))?
        .iter()
        .filter(|relation| relation["kind"] == kind)
        .cloned()
        .collect::<Vec<_>>();
    relations.sort_by_key(Value::to_string);
    Ok(relations)
}

fn mutations(
    machine_id: &str,
    user_id: &str,
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
                        if source(agentlaw_storage::version(unit, h))? == reference.observed_version
                        {
                            parent_sets.push(stored_relations(h, kind)?);
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
        let mut metadata = json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":canonical_scope(a)?,"origin":{"machine_id":machine_id,"project_id":p.context.project_id,"user_id":user_id},"recorded_at_ms":now_ms(),"is_rule":proposal.is_rule.unwrap_or(false),"relations":relations,"work_targets":proposal.work_targets.clone().unwrap_or_default()});
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
                        body: proposal_body(proposal, &[])?.to_owned(),
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

fn describe_proposal(
    detail: &mut Value,
    proposal: &MemoryProposal,
    location: ProposalLocation,
    input_index: Option<usize>,
    stored: bool,
) {
    detail["proposal_index"] = json!(location.index);
    if let Some(id) = proposal.proposal_id.as_ref().filter(|_| stored) {
        detail["proposal_id"] = json!(id);
    }
    if let Some(index) = input_index {
        detail["path"] = json!(location.input_path(index));
    } else {
        detail["submitted_proposal"] = json!(proposal);
    }
}

fn prepare_remember(access: &mut impl Preparation, r: RememberRequest) -> Result<Prepared> {
    if r.procedure.is_some() {
        return prepare_procedure(access, &r);
    }
    let mut old_version = None;
    let mut stored_proposals = 0;
    let positions;
    let mut retained = if let Some(reference) = &r.pending_batch_ref {
        let (mut retained, status, result) = access.load(reference)?;
        match r.pending_action.as_ref() {
            Some(PendingAction::Inspect) => {
                return if status == "published" {
                    decode(result.as_deref().unwrap_or("null")).map(Prepared::Replay)
                } else {
                    Ok(Prepared::Replay(pending_response(&retained, &status)))
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
                return Ok(Prepared::Discard(DiscardPlan {
                    reference: reference.clone(),
                    reason: r.discard_reason.clone().unwrap(),
                }));
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
            return decode(result.as_deref().unwrap_or("null")).map(Prepared::Replay);
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
        stored_proposals = retained.batch.proposals.len();
        if let Some(delta) = &r.memories {
            if retained.operation_id.is_some() {
                return Err(DomainError::new(
                    "publication_in_progress",
                    "Resume the retained execution before editing its proposals.",
                ));
            }
            (retained.batch, positions) =
                retained
                    .batch
                    .apply_delta_with_positions(&retained.context, reference, delta)?;
        } else {
            positions = vec![None; retained.batch.proposals.len()];
        }
        retained
    } else {
        let context = access.context(r.project_path.as_deref())?;
        let proposals = r
            .memories
            .ok_or_else(|| DomainError::new("invalid_input", "memories are required."))?;
        positions = (0..proposals.len()).map(Some).collect();
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
        return Ok(Prepared::Remember(RememberPlan {
            retained,
            expectation: PendingExpectation::Version(old_version.unwrap()),
            review_basis: None,
        }));
    }
    let mut snapshot = access.material(&retained.context, &retained.batch.proposals)?;
    struct StructuralOnly;
    impl ReviewGate for StructuralOnly {
        fn review(&self, _: &RequestContext, _: &[MemoryProposal], _: &ReadSet) -> Result<()> {
            Ok(())
        }
    }
    let expectation = old_version.map_or(PendingExpectation::New, PendingExpectation::Version);
    let prepared = match preflight_with_relations(
        &snapshot,
        &StructuralOnly,
        &retained.context,
        &retained.batch.proposals,
        |parent, kind| {
            let reference = &parent.memory_ref;
            for unit in snapshot
                .units
                .iter()
                .filter(|unit| unit.entity_id == reference.memory_id)
            {
                for head in unit.heads() {
                    let version = source(agentlaw_storage::version(unit, head))?;
                    if snapshot.streaming.actual_version(version) == reference.observed_version {
                        return stored_relations(head, kind).map(Value::Array);
                    }
                }
            }
            Err(DomainError::new(
                "source_changed",
                "Parent head disappeared from the prepared snapshot.",
            ))
        },
    ) {
        Ok(prepared) => prepared,
        Err(PreflightError::Stale(error, locations)) => {
            let mut issues = Vec::new();
            for location in locations {
                let proposal = &retained.batch.proposals[location.index];
                let ids: BTreeSet<_> = proposal
                    .parent_refs
                    .iter()
                    .flatten()
                    .chain(proposal.consolidation_refs.iter().flatten())
                    .map(|reference| &reference.memory_id)
                    .collect();
                let mut current_states = Vec::new();
                for id in ids {
                    if let Some(state) = snapshot.current(id)? {
                        current_states.push(external_current(&state));
                    }
                }
                let mut issue = json!({
                    "code":error.code,
                    "message":error.message,
                    "submitted_proposal":proposal,
                    "current_states":current_states,
                });
                describe_proposal(
                    &mut issue,
                    proposal,
                    location,
                    positions[location.index],
                    true,
                );
                issues.push(issue);
            }
            retained.issues = issues;
            retained.basis.clear();
            retained.overlap_approved = false;
            retained.resolution_ref = Some(uuid::Uuid::new_v4().to_string());
            retained.batch.reference.observed_version = uuid::Uuid::new_v4().to_string();
            return Ok(Prepared::Remember(RememberPlan {
                retained,
                expectation,
                review_basis: Some(snapshot.publication_basis),
            }));
        }
        Err(PreflightError::Located(error, details)) => {
            let details: Vec<_> = details
                .into_iter()
                .map(|mut detail| {
                    let index = detail.location.index;
                    describe_proposal(
                        &mut detail.value,
                        &retained.batch.proposals[index],
                        detail.location,
                        positions[index],
                        index < stored_proposals,
                    );
                    detail.value
                })
                .collect();
            return Err(error.with_details(json!(details)));
        }
        Err(PreflightError::Domain(error)) => return Err(error),
    };
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
            || (retained.resolution_ref.is_some() && r.resolution_ref == retained.resolution_ref));
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
    let (overlaps, diagnostics) = access.overlaps(&snapshot, &prepared, &affected)?;
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
        retained.resolution_ref = (!overlaps.is_empty()).then(|| uuid::Uuid::new_v4().to_string());
    }
    retained.batch.reference.observed_version = uuid::Uuid::new_v4().to_string();
    if retained.issues.is_empty() {
        retained.mutations = mutations(
            &access.identity().machine_id,
            &access.identity().user_id,
            &prepared,
            &retained.batch.proposals,
            &snapshot,
        )?;
        retained.operation_id = Some(uuid::Uuid::new_v4().to_string());
        retained.expected_generation = Some(snapshot.generation);
        retained.publication_basis = Some(snapshot.publication_basis.clone());
    }
    Ok(Prepared::Remember(RememberPlan {
        retained,
        expectation,
        review_basis: Some(snapshot.publication_basis),
    }))
}

fn prepare_procedure(access: &mut impl Preparation, r: &RememberRequest) -> Result<Prepared> {
    let previous: Option<(String, String, Option<String>)> = if let Some(token) = &r.authoring_ref {
        access.authoring_result(token)?
    } else {
        None
    };
    if let Some((status, submission, Some(result))) = &previous {
        if status == "published"
            && decode::<RememberRequest>(submission).and_then(|stored| encode(&stored))?
                == encode(r)?
        {
            return decode(result).map(Prepared::Replay);
        }
    }
    let p = r.procedure.as_ref().unwrap();
    let context = access.context(r.project_path.as_deref())?;
    let procedure_ids = p
        .parent_refs
        .as_ref()
        .map(|refs| {
            refs.iter()
                .map(|r| r.procedure_id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let snapshot = access.snapshot(&p.evidence_memory_ids, &procedure_ids)?;
    let mut evidence = Vec::new();
    for (index, id) in p.evidence_memory_ids.iter().enumerate() {
        validate_id(id)?;
        evidence.push(snapshot.current(id)?.ok_or_else(|| {
            validation::identity_input_error(
                "not_found",
                &format!("/procedure/evidence_memory_ids/{index}"),
                &json!(id),
            )
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
                    validation::identity_input_error(
                        "not_found",
                        "/procedure/parent_refs/0/procedure_id",
                        &json!(id),
                    )
                })?;
            let actual: BTreeSet<_> = source(unit.references())?
                .into_iter()
                .map(|r| snapshot.streaming.actual_version(r.observed_version))
                .collect();
            let submitted: BTreeSet<_> = refs.iter().map(|r| r.observed_version.clone()).collect();
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
        access.identity().binding_id,
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
        access
            .authoring_basis(token)?
            .is_some_and(|stored| stored == basis)
    } else {
        false
    };
    if p.instructions.is_none() || !valid || stale_parents {
        let token = uuid::Uuid::new_v4();

        let current = snapshot
            .units
            .iter()
            .filter(|u| {
                u.entity_type == "learned_procedure" && Some(&u.entity_id) == procedure_id.as_ref()
            })
            .map(|u| procedure_output(u, true))
            .collect::<Result<Vec<_>>>()?;
        let query = evidence
            .iter()
            .flat_map(|state| state.heads.iter())
            .map(|head| snapshot.streaming.excerpt(&head.what_to_remember, 512))
            .collect::<Result<Vec<_>>>()?
            .join("\n");
        let nearby = access.nearby(&query, r.project_path.as_deref())?;
        let mut response = json!({"status":"authoring_required","authoring_ref":token.to_string(),"builtin_instructions":"Write a complete reusable instruction-only procedure. State when to use it, preserve conditions and exceptions supported by the supplied memories, distinguish observed evidence from hypotheses, and avoid executable installation scripts. Review every parent head. Nearby entries are descriptors only: use exact procedure_ids recall to read their full instructions before relying on them.","evidence_memories":evidence.iter().map(external_current).collect::<Vec<_>>(),"current_procedures":current,"nearby_procedures":nearby["procedures"],"diagnostics":nearby["diagnostics"]});
        if let Some((status, submission, result)) = previous {
            response["retained_submission"] = decode::<Value>(&submission)?;
            response["previous_execution_status"] = json!(status);
            if let Some(result) = result {
                response["previous_execution_result"] = decode::<Value>(&result)?;
            }
            response["turn_instruction"]=json!("Inspect the retained submission and current evidence. Revise or resubmit the complete procedure using this fresh authoring_ref; the previous authorization is not reused.");
        }
        let response =
            snapshot
                .streaming
                .finish(response, &access.identity().local, access.request())?;
        return Ok(Prepared::Authoring(AuthoringPlan {
            token,
            basis,
            response,
        }));
    }
    let applicability = applies
        .ok_or_else(|| DomainError::new("invalid_input", "Final create requires applies_to."))?;
    let id = procedure_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let metadata = json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":canonical_scope(&applicability)?,"origin":{"machine_id":access.identity().machine_id,"project_id":context.project_id,"user_id":access.identity().user_id},"recorded_at_ms":now_ms(),"name":p.name,"use_when":p.use_when,"evidence_memory_ids":p.evidence_memory_ids});
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
        evidence: p
            .evidence
            .clone()
            .ok_or_else(|| DomainError::new("invalid_input", "Procedure evidence is required."))?,
    };
    Ok(Prepared::Procedure(ProcedurePlan {
        token: r.authoring_ref.clone().ok_or_else(|| {
            DomainError::new("stale_authoring_ref", "Authoring context is missing.")
        })?,
        operation_id: uuid::Uuid::new_v4(),
        generation: snapshot.generation,
        mutation,
        submission: encode(r)?,
    }))
}

#[cfg(test)]
mod tests;
