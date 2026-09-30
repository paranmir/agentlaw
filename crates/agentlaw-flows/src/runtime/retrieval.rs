//! Source-exact reads following disk-backed membership and discovery selection.
use super::*;
use crate::exact::ExactIndex;
use agentlaw_storage::published::PublishedChangeSource;
pub(super) fn pump(
    worker: &agentlaw_worker::process::Client,
    adapter: &crate::derived::PublishedAdapter<agentlaw_storage::published::OwnedPublishedReader>,
    binding: &str,
    local: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    use agentlaw_worker::derived::{DerivedContext, SourcePosition};
    let model = worker
        .model_digest()
        .map_err(|_| DomainError::new("indexing_pending", "Embedding model is not ready."))?;
    let position = adapter
        .position()
        .map_err(|_| DomainError::new("source_unavailable", "Cannot inspect derived source."))?;
    let context = DerivedContext {
        repository_id: binding.into(),
        initial_basis: SourcePosition {
            epoch: position.epoch.clone(),
            sequence: 0,
        },
        model_digest: model,
        config_digest: format!("agentlaw-current-head-and-procedure-v4:{}", position.epoch),
    };
    bootstrap(worker, adapter, &context, local, cancel)?;
    let mut cursor = worker
        .derived_position(&context)
        .map_err(|_| DomainError::new("indexing_pending", "Cannot inspect indexing cursor."))?;
    while cursor.sequence < position.sequence {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(DomainError::new("cancelled", "Index feeding cancelled."));
        }
        let page = adapter.spooled_page(worker, &cursor).map_err(|_| {
            DomainError::new(
                "indexing_coverage_unknown",
                "Source ledger coverage cannot be established.",
            )
        })?;
        cursor = match worker.ingest_spooled(&context, &page) {
            Ok(next) => next,
            Err(_) => {
                let next = worker.derived_position(&context).map_err(|_| {
                    DomainError::new("indexing_pending", "Durable indexing acceptance failed.")
                })?;
                if next.epoch != cursor.epoch || next.sequence <= cursor.sequence {
                    return Err(DomainError::new(
                        "indexing_pending",
                        "Durable indexing acceptance failed.",
                    ));
                }
                next
            }
        };
    }
    Ok(())
}
fn bootstrap(
    worker: &agentlaw_worker::process::Client,
    adapter: &crate::derived::PublishedAdapter<agentlaw_storage::published::OwnedPublishedReader>,
    context: &agentlaw_worker::derived::DerivedContext,
    local: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    let key = digest(context)?;
    let connection = sql(Connection::open(local.join("bootstrap.sqlite")))?;
    sql(connection.busy_timeout(std::time::Duration::from_secs(30)))?;
    sql(connection.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;CREATE TABLE IF NOT EXISTS bootstrap_state(context TEXT PRIMARY KEY,page INTEGER NOT NULL,after_key TEXT,pending TEXT);"))?;
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(DomainError::new(
                "cancelled",
                "Initial inventory transfer cancelled; checkpoint retained.",
            ));
        }
        // Only this rebuildable adapter checkpoint is locked, never canonical
        // source or pending-write state. Durable IPC does not wait for inference.
        sql(connection.execute_batch("BEGIN IMMEDIATE"))?;
        let step = (|| {
            let (next, complete) = worker.bootstrap_status(context).map_err(|_| {
                DomainError::new(
                    "indexing_pending",
                    "Cannot inspect initial inventory acceptance.",
                )
            })?;
            if complete {
                return Ok(true);
            }
            sql(connection.execute(
                "INSERT OR IGNORE INTO bootstrap_state VALUES(?1,0,NULL,NULL)",
                [&key],
            ))?;
            let (page, after, pending): (u64, Option<String>, Option<String>) = sql(connection
                .query_row(
                    "SELECT page,after_key,pending FROM bootstrap_state WHERE context=?1",
                    [&key],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                ))?;
            if pending.is_none() && next != page {
                return Err(DomainError::new("indexing_checkpoint_missing","Worker inventory progressed without its local transfer checkpoint; preserve both stores for recovery."));
            }
            let transfer: Value = if let Some(pending) = pending {
                decode(&pending)?
            } else {
                let (body, after, final_page) = adapter
                    .bootstrap_page(worker, &context.initial_basis, after.as_deref())
                    .map_err(|_| {
                        DomainError::new(
                            "indexing_pending",
                            "Cannot acquire initial inventory page.",
                        )
                    })?;
                let transfer = json!({"body":body,"after":after,"final":final_page});
                sql(connection.execute(
                    "UPDATE bootstrap_state SET pending=?1 WHERE context=?2",
                    params![encode(&transfer)?, key],
                ))?;
                transfer
            };
            // Commit the immutable retry manifest before invoking the receiver.
            sql(connection.execute_batch("COMMIT;BEGIN IMMEDIATE"))?;
            let actual_page: u64 = sql(connection.query_row(
                "SELECT page FROM bootstrap_state WHERE context=?1",
                [&key],
                |r| r.get(0),
            ))?;
            if actual_page != page {
                return Ok(false);
            }
            let transfer: Value = sql(connection.query_row(
                "SELECT pending FROM bootstrap_state WHERE context=?1",
                [&key],
                |r| r.get::<_, Option<String>>(0),
            ))?
            .map(|s| decode(&s))
            .transpose()?
            .unwrap_or(transfer);
            let body = serde_json::from_value(transfer["body"].clone()).map_err(|_| {
                DomainError::new(
                    "control_corrupt",
                    "Initial inventory transfer manifest is invalid.",
                )
            })?;
            worker
                .bootstrap_spooled(context, page, transfer["final"] == true, &body)
                .map_err(|_| {
                    DomainError::new(
                        "indexing_pending",
                        "Initial inventory page remains retained for retry.",
                    )
                })?;
            sql(connection.execute(
                "UPDATE bootstrap_state SET page=?1,after_key=?2,pending=NULL WHERE context=?3",
                params![page + 1, transfer["after"].as_str(), key],
            ))?;
            Ok(transfer["final"] == true)
        })();
        match step {
            Ok(done) => {
                sql(connection.execute_batch("COMMIT"))?;
                if done {
                    return Ok(());
                }
            }
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.pump_stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

pub(super) struct IndexedSearch {
    queries: BTreeMap<String, Discovery>,
    pub(super) procedure_order: Vec<String>,
    pub(super) full_procedure: Option<String>,
}
struct Channels {
    hits: Vec<agentlaw_search::Hit>,
    lexical_ids: BTreeSet<String>,
    lexical: Vec<agentlaw_search::Hit>,
    vector: Vec<agentlaw_search::Hit>,
    complete: bool,
    diagnostics: Vec<DomainError>,
}
pub(super) fn requires_overlap(
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
fn admitted(channels: &Channels) -> Option<String> {
    if !channels.complete {
        return None;
    }
    let (a, am) = winner(&channels.lexical, 0.90, 0.05)?;
    let (b, bm) = winner(&channels.vector, 0.92, 0.03)?;
    (a == b && (am || bm)).then(|| a.to_owned())
}
fn query_records(body: &str) -> Vec<&str> {
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
impl RecallSearch for IndexedSearch {
    fn search(&self, _: &RequestContext, q: &str, _: &[String]) -> Result<Discovery> {
        let d = self.queries.get(q).ok_or_else(|| {
            DomainError::new(
                "search_basis_changed",
                "The prepared query was not part of this read view.",
            )
        })?;
        Ok(Discovery {
            candidates: d.candidates.clone(),
            full_ids: d.full_ids.clone(),
            diagnostics: d.diagnostics.clone(),
        })
    }
}
impl Runtime {
    pub(super) fn overlap_candidates(
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
    fn search_channels(
        &self,
        index: &ExactIndex,
        c: &RequestContext,
        query: &str,
        limit: usize,
    ) -> Result<Channels> {
        self.search_scopes_channels(index, &crate::exact::allowed_scopes(c), query, limit)
    }
    fn search_scopes_channels(
        &self,
        index: &ExactIndex,
        scopes: &[String],
        query: &str,
        limit: usize,
    ) -> Result<Channels> {
        let lexical = index.search_scopes(query, scopes, limit)?;
        let fallback = |message: &str| Channels {
            hits: lexical.clone(),
            lexical_ids: lexical.iter().map(|h| h.memory_id.clone()).collect(),
            lexical: vec![],
            vector: vec![],
            complete: false,
            diagnostics: vec![DomainError::new("semantic_channel_incomplete", message)],
        };
        let Some(worker) = self.worker.as_ref() else {
            return Ok(fallback("No semantic worker is attached; exact and indexed lexical retrieval remain available."));
        };
        if !matches!(
            worker.availability(),
            Ok(agentlaw_worker::SemanticAvailability::Ready)
        ) {
            return Ok(fallback("The semantic worker is not ready; exact and indexed lexical retrieval remain available."));
        }
        let semantic = (|| {
            pump(
                worker,
                &crate::derived::PublishedAdapter::new(
                    self.store.owned_published_reader(),
                    self.binding_id.clone(),
                ),
                &self.binding_id,
                &self.local,
                &self.request_control.cancel,
            )?;
            self.request_control.check()?;
            let p = index.position()?.unwrap();
            let context = agentlaw_worker::derived::DerivedContext {
                repository_id: self.binding_id.clone(),
                initial_basis: agentlaw_worker::derived::SourcePosition {
                    epoch: p.epoch.clone(),
                    sequence: 0,
                },
                model_digest: worker.model_digest().map_err(|_| {
                    DomainError::new("semantic_failed", "Cannot inspect semantic model.")
                })?,
                config_digest: format!("agentlaw-current-head-and-procedure-v4:{}", p.epoch),
            };
            let vector = worker
                .embed_cancellable(query, &self.request_control.cancel)
                .map_err(|_| DomainError::new("semantic_failed", "Cannot embed the query."))?;
            worker
                .search_index_cancellable(
                    &context,
                    query,
                    scopes,
                    limit,
                    Some(&vector),
                    agentlaw_worker::derived::SourcePosition {
                        epoch: p.epoch,
                        sequence: p.sequence,
                    },
                    &self.request_control.cancel,
                )
                .map_err(|_| {
                    DomainError::new(
                        "semantic_failed",
                        "Cannot read the required semantic index view.",
                    )
                })
        })();
        self.request_control.check()?;
        match semantic {
            Ok(packet) => {
                let lexical_ids = packet.lexical.iter().map(|h| h.memory_id.clone()).collect();
                let hits = agentlaw_search::reciprocal_rank_fusion(&[
                    packet.lexical,
                    packet.vector.clone(),
                ]);
                Ok(Channels {
                    hits,
                    lexical_ids,
                    lexical: packet.lexical_strength,
                    vector: packet.vector,
                    complete: packet.semantic_complete,
                    diagnostics: if packet.semantic_complete {
                        vec![]
                    } else {
                        vec![DomainError::new(
                            "semantic_channel_incomplete",
                            "The worker did not establish a complete semantic view.",
                        )]
                    },
                })
            }
            Err(_) => Ok(fallback(
                "Semantic retrieval failed; exact and indexed lexical retrieval remain available.",
            )),
        }
    }
    fn exact_index(&self) -> Result<ExactIndex> {
        let filename: Option<String> = sql(self
            .control
            .query_row(
                "SELECT value FROM settings WHERE key='exact_active_generation'",
                [],
                |r| r.get(0),
            )
            .optional())?;
        let mut index = ExactIndex::open(
            self.local
                .join(filename.unwrap_or_else(|| "exact-v3.sqlite".into())),
        )?;
        if index.position()?.is_none() {
            index.rebuild(&self.store, &self.request_control)?;
            return Ok(index);
        }
        match index.synchronize(&self.store, &self.request_control) {
            Ok(_) => {}
            Err(e)
                if e.code == "exact_index_rebuild_required"
                    || e.code == "exact_index_coverage_unknown" =>
            {
                index.rebuild(&self.store, &self.request_control)?;
            }
            Err(e) => return Err(e),
        }
        Ok(index)
    }
    pub(super) fn snapshot_selected(
        &self,
        ids: &[String],
        procedures: &[String],
    ) -> Result<Snapshot> {
        self.request_control.check()?;
        let (position, resolved, missing) = source(self.store.read_closure(ids))?;
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
            match self.store.read_procedure(id) {
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
        if source(self.store.published_reader().position())? != position {
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
    pub(super) fn recall_material(
        &self,
        r: &RecallRequest,
        c: &RequestContext,
    ) -> Result<(Snapshot, IndexedSearch)> {
        if r.recall_for.is_none() {
            return Ok((
                self.snapshot_for_recall(
                    r.memory_ids.as_deref().unwrap_or(&[]),
                    r.procedure_ids.as_deref().unwrap_or(&[]),
                )?,
                IndexedSearch {
                    queries: BTreeMap::new(),
                    procedure_order: vec![],
                    full_procedure: None,
                },
            ));
        }
        let index = self.exact_index()?;
        let position = index.position()?.unwrap();
        let mut ids: BTreeSet<String> = r
            .memory_ids
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut previews = BTreeSet::new();
        let mut queries = BTreeMap::new();
        if let Some(query) = &r.recall_for {
            let mut texts = vec![query.clone()];
            if r.restore_context == Some(true) {
                texts.extend(
                    RESTORE_PERSPECTIVES
                        .iter()
                        .map(|p| format!("{query}\nPerspective: {p}")),
                )
            }
            for q in texts {
                self.request_control.check()?;
                let combined = if let Some(terms) = &r.search_terms {
                    format!("{q} {}", terms.join(" "))
                } else {
                    q.clone()
                };
                let channels = self.search_channels(&index, c, &combined, 10000)?;
                previews.extend(channels.hits.iter().map(|h| h.memory_id.clone()));
                ids.extend(admitted(&channels));
                queries.insert(q, channels);
            }
            ids.extend(index.ids_matching(c, true, r.include_active_tasks == Some(true))?);
            if let Some(targets) = &r.work_targets {
                let project = c.project_id.as_deref().ok_or_else(|| {
                    DomainError::new(
                        "project_connection_required",
                        "Work targets require a connected project.",
                    )
                })?;
                for (id, reading) in index.work_targets(project, targets)? {
                    if reading == Reading::Required {
                        ids.insert(id);
                    } else {
                        previews.insert(id);
                    }
                }
            }
        }
        let discovery: Vec<_> = queries
            .values()
            .flat_map(|c| &c.hits)
            .map(|h| h.memory_id.clone())
            .collect();
        previews.extend(index.related(&discovery)?.into_iter().map(|x| x.1));
        let mut procedures: BTreeSet<_> = r
            .procedure_ids
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut procedure_order = Vec::new();
        let mut full_procedure = None;
        if let Some(query) = &r.recall_for {
            let scopes: Vec<_> = crate::exact::allowed_scopes(c)
                .into_iter()
                .map(|s| format!("procedure:{s}"))
                .collect();
            let channels = self.search_scopes_channels(&index, &scopes, query, 10000)?;
            full_procedure =
                admitted(&channels).and_then(|id| id.strip_prefix("procedure/").map(str::to_owned));
            for hit in channels.hits {
                if let Some(id) = hit.memory_id.strip_prefix("procedure/") {
                    procedure_order.push(id.into());
                }
            }
        }
        procedures.extend(full_procedure.iter().cloned());
        let mut snapshot = self.snapshot_for_recall(
            &ids.into_iter().collect::<Vec<_>>(),
            &procedures.into_iter().collect::<Vec<_>>(),
        )?;
        let mut projected: BTreeMap<String, Vec<crate::exact::HeadRecord>> = BTreeMap::new();
        for head in index.heads(&previews.into_iter().collect::<Vec<_>>())? {
            projected
                .entry(head.entity_id.clone())
                .or_default()
                .push(head);
        }
        for (id, heads) in projected {
            if snapshot.states.contains_key(&id) {
                continue;
            }
            let unit = CurrentUnit {
                entity_id: id.clone(),
                entity_type: "memory".into(),
                state: UnitState::Live { heads: vec![] },
            };
            let memories = heads
                .into_iter()
                .map(|head| {
                    let mut memory = memory_from(
                        &unit,
                        &Head {
                            metadata: head.metadata,
                            body: head.excerpt,
                        },
                    )?;
                    memory.memory_ref.observed_version = head.observed_version;
                    Ok(memory)
                })
                .collect::<Result<Vec<_>>>()?;
            snapshot.states.insert(
                id.clone(),
                CurrentState {
                    requested_id: id.clone(),
                    resolved_id: id,
                    heads: memories,
                    redirect_path: vec![],
                },
            );
        }
        let mut descriptors: BTreeMap<String, Vec<Head>> = BTreeMap::new();
        for head in index.heads(&procedure_order)? {
            if snapshot
                .units
                .iter()
                .any(|u| u.entity_type == "learned_procedure" && u.entity_id == head.entity_id)
            {
                continue;
            }
            descriptors.entry(head.entity_id).or_default().push(Head {
                metadata: head.metadata,
                body: String::new(),
            });
        }
        for (id, heads) in descriptors {
            snapshot.units.push(CurrentUnit {
                entity_id: id,
                entity_type: "learned_procedure".into(),
                state: UnitState::Live { heads },
            });
        }
        if snapshot.generation != position.sequence
            || source(self.store.owned_published_reader().position())? != position
        {
            return Err(DomainError::new(
                "source_changed",
                "Source changed after discovery selection; repeat the request.",
            ));
        }
        let mut prepared = BTreeMap::new();
        for (q, channels) in queries {
            let full_ids = admitted(&channels).into_iter().collect();
            let truncated = channels.hits.len() >= 10000;
            let mut candidates = Vec::new();
            for hit in channels.hits.into_iter().take(10000) {
                if let Some(state) = snapshot.current(&hit.memory_id)? {
                    for head in state
                        .heads
                        .iter()
                        .filter(|h| scope_matches(&h.applicability, c))
                    {
                        let excerpt = snapshot.streaming.excerpt(&head.what_to_remember, 400)?;
                        candidates.push(MemoryCandidate {
                            memory_id: state.resolved_id.clone(),
                            excerpt: excerpt.clone(),
                            applicability: head.applicability.clone(),
                            retrieval_paths: [
                                ("lexical", channels.lexical_ids.contains(&hit.memory_id)),
                                (
                                    "vector",
                                    channels.vector.iter().any(|h| h.memory_id == hit.memory_id),
                                ),
                            ]
                            .into_iter()
                            .filter(|(_, found)| *found)
                            .map(|(via, _)| RetrievalPath {
                                via: vec![via.into()],
                                clue: excerpt.chars().take(80).collect(),
                                source_memory_id: None,
                            })
                            .collect(),
                        })
                    }
                }
            }
            let mut diagnostics = channels.diagnostics;
            if truncated {
                diagnostics.push(DomainError::new("candidate_coverage_incomplete","Candidate retrieval reached its 10000-ID ceiling; the returned count is a lower bound, not a complete corpus count."));
            }
            prepared.insert(
                q,
                Discovery {
                    candidates,
                    full_ids,
                    diagnostics,
                },
            );
        }
        Ok((
            snapshot,
            IndexedSearch {
                queries: prepared,
                procedure_order,
                full_procedure,
            },
        ))
    }
    pub(super) fn write_material(
        &self,
        c: &RequestContext,
        proposals: &[MemoryProposal],
    ) -> Result<Snapshot> {
        let index = self.exact_index()?;
        let position = index.position()?.unwrap();
        let mut ids = BTreeSet::new();
        let mut affected = BTreeSet::new();
        for p in proposals {
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
            let mut context = c.clone();
            if p.applies_to.is_none() {
                if let Some(reference) = p.parent_refs.iter().flatten().next() {
                    if let Some(head) = index.heads(&[reference.memory_id.clone()])?.first() {
                        let a = scope_value(&head.metadata["applicability"])?;
                        context.project_id = a.project_id;
                        if let Some(machine) = a.machine_id {
                            context.machine_id = machine;
                        }
                    }
                }
            }
            if p.operation == Operation::Create || p.applies_to.is_some() {
                ids.extend(index.identical_bodies(&p.what_to_remember)?);
                for record in query_records(&p.what_to_remember) {
                    ids.extend(
                        self.search_channels(&index, &context, record, 128)?
                            .hits
                            .into_iter()
                            .map(|h| h.memory_id),
                    );
                }
            }
        }
        let affected = affected.into_iter().collect::<Vec<_>>();
        ids.extend(index.related(&affected)?.into_iter().map(|x| x.1));
        ids.extend(index.reverse_required(&affected)?);
        let mut snapshot = self.snapshot_selected(&ids.into_iter().collect::<Vec<_>>(), &[])?;
        for id in affected {
            snapshot
                .publication_basis
                .reverse_required
                .insert(id.clone(), index.reverse_required(&[id])?);
        }
        if snapshot.generation != position.sequence {
            return Err(DomainError::new(
                "source_changed",
                "Source changed during write candidate selection; repeat review.",
            ));
        }
        Ok(snapshot)
    }
    pub fn prepare_for_connection(&mut self, control: RequestControl) -> Result<Value> {
        let old = std::mem::replace(&mut self.request_control, control);
        let result = (|| {
            self.request_control.phase("building_lexical_index");
            let index = self.exact_index()?;
            if let Some(worker) = &self.worker {
                loop {
                    self.request_control.check()?;
                    match worker.availability() {
                        Ok(agentlaw_worker::SemanticAvailability::Loading) => {
                            self.request_control.phase("loading_semantic_model");
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                        Ok(agentlaw_worker::SemanticAvailability::Ready) => break,
                        Ok(state) => {
                            return Ok(
                                json!({"lexical_complete":true,"semantic_complete":false,"diagnostics":[{"code":"semantic_unavailable","message":format!("Semantic model state is {state:?}; no vector build completion is claimed.")}]}),
                            )
                        }
                        Err(_) => {
                            return Err(DomainError::new(
                                "semantic_failed",
                                "Cannot inspect configured semantic worker during connection.",
                            ))
                        }
                    }
                }
                self.request_control.phase("building_vector_index");
                let context = resolve_context(self, None)?;
                let channels =
                    self.search_channels(&index, &context, "connection index fence", 1)?;
                if !channels.complete {
                    return Err(DomainError::new("semantic_build_incomplete","Configured semantic index did not complete; connection selection must remain unchanged."));
                }
                Ok(json!({"lexical_complete":true,"semantic_complete":true}))
            } else {
                Ok(
                    json!({"lexical_complete":true,"semantic_complete":false,"diagnostics":[{"code":"semantic_unavailable","message":"No model is configured; vector build was not performed."}]}),
                )
            }
        })();
        self.request_control = old;
        result
    }
    pub fn repair_derived(&mut self, control: RequestControl) -> Result<Value> {
        let old = std::mem::replace(&mut self.request_control, control);
        let result = (|| {
            self.request_control
                .phase("rebuilding_private_derived_generation");
            let filename = format!("exact-repair-{}.sqlite", uuid::Uuid::new_v4());
            let mut index = ExactIndex::open(self.local.join(&filename))?;
            let position = index.rebuild(&self.store, &self.request_control)?;
            let mut semantic = false;
            let mut worker_receipt = None;
            if let Some(worker) = &self.worker {
                self.request_control.phase("repairing_worker_generation");
                if matches!(
                    worker.availability(),
                    Ok(agentlaw_worker::SemanticAvailability::Ready)
                ) {
                    let context = agentlaw_worker::derived::DerivedContext {
                        repository_id: self.binding_id.clone(),
                        initial_basis: agentlaw_worker::derived::SourcePosition {
                            epoch: position.epoch.clone(),
                            sequence: 0,
                        },
                        model_digest: worker.model_digest().map_err(|_| {
                            DomainError::new(
                                "semantic_failed",
                                "Cannot inspect model for derived repair.",
                            )
                        })?,
                        config_digest: format!(
                            "agentlaw-current-head-and-procedure-v4:{}",
                            position.epoch
                        ),
                    };
                    self.enqueue_derived()?;
                    worker_receipt=Some(worker.repair_index(&context).map_err(|_|DomainError::new("derived_repair_failed","Worker generation repair failed; prior generations and pending work are retained."))?);
                    let channels = self.search_channels(
                        &index,
                        &resolve_context(self, None)?,
                        "repair index fence",
                        1,
                    )?;
                    if !channels.complete {
                        return Err(DomainError::new("derived_repair_incomplete","Configured semantic index has not completed its repaired source fence."));
                    }
                    semantic = true;
                }
            }
            self.request_control.check()?;
            sql(self.control.execute("INSERT INTO settings(key,value) VALUES('exact_active_generation',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[filename]))?;
            Ok(
                json!({"lexical_complete":true,"semantic_complete":semantic,"worker_generation":worker_receipt,"diagnostics":if semantic{vec![]}else{vec![DomainError::new("semantic_unavailable","No ready semantic model was available; exact/lexical repair completed but no vector completion is claimed.")]} }),
            )
        })();
        self.request_control = old;
        result
    }
    pub fn search_procedures(
        &self,
        query: &str,
        project_path: Option<&str>,
        limit: usize,
    ) -> Result<Value> {
        let index = self.exact_index()?;
        let scopes = if let Some(path) = project_path {
            crate::exact::allowed_scopes(&resolve_context(self, Some(path))?)
        } else {
            index.all_scopes()?
        }
        .into_iter()
        .map(|s| format!("procedure:{s}"))
        .collect::<Vec<_>>();
        self.procedure_search_scopes(&index, query, &scopes, limit)
    }
    fn procedure_search_scopes(
        &self,
        index: &ExactIndex,
        query: &str,
        scopes: &[String],
        limit: usize,
    ) -> Result<Value> {
        let channels = self.search_scopes_channels(index, scopes, query, 10000)?;
        let matched = channels.hits.len();
        let mut results = Vec::new();
        let mut shown = 0;
        for hit in channels.hits.into_iter().take(limit) {
            let Some(id) = hit.memory_id.strip_prefix("procedure/") else {
                continue;
            };
            let mut descriptors = Vec::new();
            for h in index.heads(&[id.to_owned()])? {
                let applicability = scope_value(&h.metadata["applicability"])?;
                if !scopes.contains(&format!("procedure:{}", scope_token(&applicability))) {
                    continue;
                }
                let value = json!({"procedure_id":id,"name":h.metadata["name"],"use_when":h.metadata["use_when"],"applicability":applicability});
                if !descriptors.contains(&value) {
                    descriptors.push(value)
                }
            }
            descriptors.sort_by_key(Value::to_string);
            if !descriptors.is_empty() {
                shown += 1;
                results.extend(descriptors)
            }
        }
        Ok(
            json!({"procedures":results,"matched":matched,"shown":shown,"diagnostics":channels.diagnostics}),
        )
    }
    pub fn search_procedures_filtered(
        &self,
        query: &str,
        scope: Option<&str>,
        project: Option<&str>,
        machine: Option<&str>,
        limit: usize,
    ) -> Result<Value> {
        let project = if let Some(selector) = project {
            let matches = if validate_id(selector).is_ok() {
                self.discover(None)?
            } else {
                self.discover(Some(&ProjectClues {
                    name: Some(selector.into()),
                    description: None,
                    repository_url: None,
                }))?
            };
            let exact = matches.iter().find(|p| p.project_id == selector);
            if let Some(p) = exact {
                Some(p.project_id.clone())
            } else if matches.len() == 1 {
                Some(matches[0].project_id.clone())
            } else {
                return Err(DomainError::new(
                    "project_selection_required",
                    "Choose one explicit project ID for management search.",
                ));
            }
        } else {
            None
        };
        if let Some(machine) = machine {
            validate_id(machine)?
        }
        let index = self.exact_index()?;
        let scopes = index
            .all_scopes()?
            .into_iter()
            .filter(|s| {
                scope.is_none_or(|kind| match kind {
                    "user" => s == "user",
                    "project" => s.starts_with("project:") && !s.contains(":machine:"),
                    "machine" => s.starts_with("machine:"),
                    "project_machine" => s.starts_with("project:") && s.contains(":machine:"),
                    _ => false,
                })
            })
            .filter(|s| {
                project.as_ref().is_none_or(|id| {
                    s == &format!("project:{id}")
                        || s.starts_with(&format!("project:{id}:machine:"))
                })
            })
            .filter(|s| {
                machine.is_none_or(|id| {
                    s == &format!("machine:{id}") || s.ends_with(&format!(":machine:{id}"))
                })
            })
            .map(|s| format!("procedure:{s}"))
            .collect::<Vec<_>>();
        self.procedure_search_scopes(&index, query, &scopes, limit)
    }
}
