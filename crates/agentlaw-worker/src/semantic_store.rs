//! Semantic index backend: search, vector flushing and index source handling.
use crate::daemon::{Response, Result, Signal};
use crate::derived::{
    DerivedContext, DerivedWorkCoordinator, PublishedPage, PublishedSourcePort, SourcePosition,
};
use crate::{Broker, EmbeddingError, SemanticAvailability};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::time::Duration;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchPacket {
    pub lexical_matched: u64,
    pub vector_candidates: u64,
    pub lexical: Vec<agentlaw_search::Hit>,
    pub lexical_strength: Vec<agentlaw_search::Hit>,
    pub vector: Vec<agentlaw_search::Hit>,
    pub lexical_stamp: agentlaw_search::ViewStamp,
    pub vector_stamp: Option<agentlaw_search::ViewStamp>,
    pub semantic_complete: bool,
    pub source_position: SourcePosition,
}
pub(crate) fn handle_bootstrap_spool(
    broker: &mut Broker,
    signal: &Signal,
    state: &std::path::Path,
    context: DerivedContext,
    page_number: u64,
    final_page: bool,
    page: crate::spool::PublishedSpoolPage,
) -> Result<Response> {
    let _ = DerivedWorkCoordinator::new(broker, &MaterializedSource(None))?;
    if crate::derived::bootstrap_receipt(
        broker,
        &context,
        page_number,
        final_page,
        &page.page,
        &page.bodies,
    )? {
        signal.notify();
        return Ok(Response::Position(context.initial_basis.clone()));
    }
    let source = MaterializedSource(Some(page.page));
    let position = DerivedWorkCoordinator::new(broker, &source)?.bootstrap_with_spools(
        &context,
        page_number,
        final_page,
        state,
        &page.bodies,
    )?;
    for reference in &page.bodies {
        let _ = crate::spool::release_body(state, &reference.body);
    }
    signal.notify();
    while crate::indexing::flush_channel(
        broker,
        &context,
        state,
        crate::indexing::Channel::Lexical,
    )? {}
    signal.notify();
    Ok(Response::Position(position))
}
pub(crate) fn handle_repair_index(
    broker: &mut Broker,
    signal: &Signal,
    state: &std::path::Path,
    context: DerivedContext,
) -> Result<Response> {
    let receipt = crate::indexing::repair_index(broker, &context, state)?;
    signal.notify();
    Ok(Response::RepairIndex(receipt))
}
pub(crate) fn handle_derived_spool(
    broker: &mut Broker,
    signal: &Signal,
    state: &std::path::Path,
    context: DerivedContext,
    page: crate::spool::PublishedSpoolPage,
) -> Result<Response> {
    if let Some(position) =
        crate::derived::accepted_receipt(broker, &context, &page.page, &page.bodies)?
    {
        signal.notify();
        return Ok(Response::Position(position));
    }
    let limit = (page.page.batches.len() as u32).max(1);
    let source = MaterializedSource(Some(page.page));
    let mut coordinator = DerivedWorkCoordinator::new(broker, &source)?;
    let position = coordinator.advance_with_spools(&context, limit, Some((state, &page.bodies)))?;
    for reference in &page.bodies {
        if let Err(error) = crate::spool::release_body(state, &reference.body) {
            broker.connection.execute("INSERT INTO diagnostics VALUES('spool_cleanup',?1) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",[error.to_string()])?;
        }
    }
    signal.notify();
    while crate::indexing::flush_channel(
        broker,
        &context,
        state,
        crate::indexing::Channel::Lexical,
    )? {}
    signal.notify();
    Ok(Response::Position(position))
}
pub(crate) fn handle_derived_submit(
    broker: &mut Broker,
    signal: &Signal,
    state: &std::path::Path,
    context: DerivedContext,
    page: PublishedPage,
) -> Result<Response> {
    if let Some(position) = crate::derived::accepted_receipt(broker, &context, &page, &[])? {
        signal.notify();
        return Ok(Response::Position(position));
    }
    let limit = (page.batches.len() as u32).max(1);
    let source = MaterializedSource(Some(page));
    let mut coordinator = DerivedWorkCoordinator::new(broker, &source)?;
    let position = coordinator.advance(&context, limit)?;
    signal.notify();
    while crate::indexing::flush_channel(
        broker,
        &context,
        state,
        crate::indexing::Channel::Lexical,
    )? {}
    signal.notify();
    Ok(Response::Position(position))
}
pub(crate) fn handle_search(
    broker: &mut Broker,
    state: &std::path::Path,
    signal: &Signal,
    context: DerivedContext,
    query: String,
    scopes: Vec<String>,
    limit: usize,
    vector: Option<Vec<f32>>,
    required: SourcePosition,
) -> Result<Response> {
    search(
        broker,
        state,
        signal,
        &context,
        &query,
        &scopes,
        limit,
        vector.as_deref(),
        required,
    )
}
pub(crate) struct MaterializedSource(pub(crate) Option<PublishedPage>);
pub(crate) fn index_background_pass(b: &mut Broker, state: &std::path::Path) -> Result<()> {
    if let Err(error) = flush_vectors(b, state) {
        b.connection.execute("INSERT INTO diagnostics VALUES('index_background',?1) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",[error.to_string()])?;
        b.connection.execute("INSERT INTO diagnostics VALUES('index_background_status','active') ON CONFLICT(name) DO UPDATE SET cause='active'",[])?;
    } else {
        b.connection.execute("UPDATE diagnostics SET cause='resolved; last failure retained in index_background' WHERE name='index_background_status'",[])?;
    }
    Ok(())
}
pub(crate) fn flush_vectors(b: &mut Broker, state: &std::path::Path) -> Result<()> {
    let stored_contexts = {
        let mut st = b.connection.prepare("SELECT json FROM derived_contexts")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let contexts: Vec<DerivedContext> = stored_contexts
        .iter()
        .map(|json| serde_json::from_str(json))
        .collect::<std::result::Result<_, _>>()?;
    let mut failed_repositories = std::collections::HashSet::new();
    let mut first_error: Option<String> = None;
    for c in &contexts {
        loop {
            match crate::indexing::flush_channel(b, c, state, crate::indexing::Channel::Vector) {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    let detail = format!(
                        "model={} config={}: {error}",
                        c.model_digest, c.config_digest
                    );
                    b.connection.execute("INSERT INTO diagnostics VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",rusqlite::params![format!("vector_index:{}",c.repository_id),detail])?;
                    failed_repositories.insert(c.repository_id.clone());
                    first_error.get_or_insert_with(|| error.to_string());
                    break;
                }
            }
        }
    }
    let repositories: std::collections::HashSet<&str> =
        contexts.iter().map(|c| c.repository_id.as_str()).collect();
    for repository in repositories {
        if failed_repositories.contains(repository) {
            continue;
        }
        let name = format!("vector_index:{repository}");
        let active: bool = b.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM diagnostics WHERE name=?1)",
            [&name],
            |r| r.get(0),
        )?;
        if !active {
            continue;
        }
        let mut verified = true;
        for c in contexts.iter().filter(|c| c.repository_id == repository) {
            match crate::indexing::verify_vector_recovery(b, c, state) {
                Ok(true) => {}
                Ok(false) => verified = false,
                Err(error) => {
                    let detail = format!(
                        "model={} config={}: {error}",
                        c.model_digest, c.config_digest
                    );
                    b.connection.execute("INSERT INTO diagnostics VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET cause=excluded.cause",rusqlite::params![name,detail])?;
                    first_error.get_or_insert_with(|| error.to_string());
                    verified = false;
                    break;
                }
            }
        }
        if verified {
            crate::supervisor::resolve_diagnostic(b, &name, None)?;
        }
    }
    if let Some(error) = first_error {
        return Err(error.into());
    }
    let unresolved: Option<String> = b
        .connection
        .query_row(
            "SELECT name FROM diagnostics WHERE name LIKE 'vector_index:%' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(name) = unresolved {
        return Err(format!("{name}: recovery not yet verified").into());
    }
    Ok(())
}
fn search(
    b: &mut Broker,
    state: &std::path::Path,
    signal: &Signal,
    c: &DerivedContext,
    query: &str,
    scopes: &[String],
    limit: usize,
    vector: Option<&[f32]>,
    required: SourcePosition,
) -> Result<Response> {
    use crate::indexing::{channel_position, flush_channel, Channel};
    use agentlaw_search::{ExactVectorIndex, ScopeFilter, SearchIndex};
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    if required.epoch != c.initial_basis.epoch {
        return Err("source epoch mismatch".into());
    }
    loop {
        let observed = signal.version();
        while flush_channel(b, c, state, Channel::Lexical)? {}
        if vector.is_some() {
            while flush_channel(b, c, state, Channel::Vector)? {}
        }
        let lex = channel_position(b, c, Channel::Lexical)?;
        let vec = channel_position(b, c, Channel::Vector)?;
        if lex.sequence >= required.sequence
            && (vector.is_none() || vec.sequence >= required.sequence)
            && crate::indexing::bootstrap_channel_complete(b, c, Channel::Lexical)?
            && (vector.is_none()
                || crate::indexing::bootstrap_channel_complete(b, c, Channel::Vector)?)
        {
            break;
        }
        if vector.is_some() && b.availability()? != SemanticAvailability::Ready {
            return Ok(Response::Error(EmbeddingError::Unavailable(
                "model not ready for requested vector fence".into(),
            )));
        }
        let Some(wait) = deadline.checked_duration_since(std::time::Instant::now()) else {
            return Ok(Response::Pending);
        };
        signal.wait(observed, wait);
    }
    let generation_lease = crate::indexing::generation_lease(state, c)?;
    let dir = generation_lease
        .as_ref()
        .map(|g| g.directory.clone())
        .unwrap_or(crate::indexing::directory(state, c)?);
    let scope = ScopeFilter {
        allowed_scopes: scopes.to_vec(),
    };
    let mut index = SearchIndex::open(dir.join("lexical.sqlite"))?;
    index.prepare_scope(&scope)?;
    let mut view = index.read_view()?;
    if view.stamp.ack < required.sequence as i64 {
        return Err("lexical immutable view does not cover source fence; rebuild required".into());
    }
    let lexical = view.search(query, &scope, limit, &[], &[], true)?;
    let lexical_strength = view.lexical_strength(query, &scope, limit)?;
    let lexical_matched = view.lexical_matched(query, &scope)?;
    let (vector, vector_stamp, vector_candidates) = if let Some(query_vector) = vector {
        let view = ExactVectorIndex::open(
            dir.join("vector.sqlite"),
            &c.model_digest,
            &c.config_digest,
            256,
        )?
        .read_view()?;
        if view.stamp.ack < required.sequence as i64 {
            return Err(
                "vector immutable view does not cover source fence; rebuild required".into(),
            );
        }
        let (hits, metrics) = view.search_ann(query_vector, &scope, limit)?;
        (
            hits,
            Some(view.stamp.clone()),
            metrics.memory_candidates as u64,
        )
    } else {
        (Vec::new(), None, 0)
    };
    Ok(Response::Search(SearchPacket {
        lexical_matched,
        vector_candidates,
        lexical,
        lexical_strength,
        vector,
        lexical_stamp: view.stamp,
        semantic_complete: vector_stamp.is_some(),
        vector_stamp,
        source_position: required,
    }))
}
impl PublishedSourcePort for MaterializedSource {
    fn read_published_changes(
        &self,
        _: &str,
        basis: &SourcePosition,
        _: u32,
    ) -> crate::derived::Result<PublishedPage> {
        let page = self.0.clone().ok_or_else(|| {
            crate::derived::Error::SourceUnavailable("no published page supplied".into())
        })?;
        if &page.basis != basis {
            return Err(crate::derived::Error::CoverageLost(
                "source page basis does not match durable queue cursor".into(),
            ));
        }
        Ok(page)
    }
}
