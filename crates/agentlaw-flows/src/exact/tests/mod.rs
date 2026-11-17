use super::*;
mod memory;
pub(super) mod oracle;
mod projection;

fn generation(dir: &Path, name: &str, pointer: &str) -> ProjectionGeneration {
    let exact = dir.join(format!("exact-{name}.sqlite"));
    let lexical = dir.join(format!("lexical-{name}.sqlite"));
    let index = ExactIndex::open(&exact).unwrap();
    index
        .connection
        .execute("INSERT INTO exact_position VALUES(1,'epoch','0')", [])
        .unwrap();
    index
        .connection
        .execute("INSERT INTO exact_lexical VALUES(1,?1)", [pointer])
        .unwrap();
    agentlaw_search::SearchIndex::open(&lexical).unwrap();
    ProjectionGeneration {
        id: name.into(),
        epoch: "epoch".into(),
        exact_file: exact,
        lexical_file: lexical,
    }
}

struct Fixture {
    store: agentlaw_storage::Store,
    writer: crate::control::ControlWriter,
    identity: crate::store::StoreIdentity,
    registry: ProjectionGenerations,
}
struct Unavailable;
impl crate::semantic::SemanticPort for Unavailable {
    fn embed_query(
        &self,
        _: &str,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::QueryVector, crate::semantic::SemanticError> {
        Err(crate::semantic::SemanticError::Unavailable(
            "fixture".into(),
        ))
    }
    fn search_available(
        &self,
        _: crate::semantic::SemanticQuery<'_>,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::SemanticCandidates, crate::semantic::SemanticError>
    {
        Err(crate::semantic::SemanticError::Unavailable(
            "fixture".into(),
        ))
    }
}
impl crate::semantic::HistorySemanticPort for Unavailable {
    fn offer(&self, _: crate::semantic::HistoryIngestHint) -> crate::semantic::OfferResult {
        crate::semantic::OfferResult::Accepted
    }
    fn search_available(
        &self,
        _: &str,
        _: &str,
        _: &crate::semantic::QueryVector,
        _: &[String],
        _: usize,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::SemanticCandidates, crate::semantic::SemanticError>
    {
        Err(crate::semantic::SemanticError::Unavailable(
            "fixture".into(),
        ))
    }
}
impl Fixture {
    fn open(root: impl AsRef<Path>, local: impl AsRef<Path>, user: &str) -> Result<Self> {
        let local = local.as_ref().to_path_buf();
        let store = agentlaw_storage::Store::open(root.as_ref(), local.join("canonical")).unwrap();
        let identity = crate::store::StoreIdentity {
            root: std::fs::canonicalize(root).unwrap(),
            local: local.clone(),
            user_id: user.into(),
            machine_id: uuid::Uuid::new_v4().to_string(),
            binding_id: uuid::Uuid::new_v4().to_string(),
            history_limit: 8192,
            response_limit: 65536,
            config_file: None,
        };
        let mut writer = crate::control::ControlWriter::open(&local)?;
        writer.bind_identity(&identity)?;
        writer.finish_binding_initialization(&identity)?;
        let shadow = ExactIndexWriter::rebuild_shadow_in(
            &local.join("projection"),
            &store.owned_published_reader(),
            &RequestControl::default(),
        )?;
        let registry = ProjectionGenerations::from_generation(shadow.generation)?;
        Ok(Self {
            store,
            writer,
            identity,
            registry,
        })
    }
    fn machine_id(&self) -> &str {
        &self.identity.machine_id
    }
    fn remember(&mut self, body: &str) {
        let request = serde_json::from_value(serde_json::json!({"memories":[{"operation":"create","what_to_remember":body,"evidence":"Observed in projection test","applies_to":["user"]}]})).unwrap();
        self.call(Request::RememberThis(request)).unwrap();
    }
    fn call(&mut self, request: Request) -> Result<serde_json::Value> {
        let source = self.store.owned_published_reader();
        let reader = crate::control::ControlReader::open(&self.identity.local).unwrap();
        let cache = crate::history_cache::HistoryCacheGuards::new();
        let control = RequestControl::default();
        let ctx = crate::store::ReadCtx {
            identity: &self.identity,
            source: &source,
            control: &reader,
            semantic: &Unavailable,
            history_semantic: &Unavailable,
            history_cache: &cache,
            request: &control,
        };
        let prepared = match request {
            Request::RememberThis(request) => {
                let guard = source.read_snapshot().unwrap();
                let mut exact =
                    ExactSnapshot::build(self.registry.pin()?, &guard, DeltaBudget::default())?;
                drop(guard);
                crate::prepare::prepare(&ctx, &mut exact, request)?
            }
            Request::ConnectProjectMemory(request) => {
                crate::prepare::prepare_connect(&ctx, request)?
            }
            _ => unreachable!("fixture only prepares writes and connections"),
        };
        drop(reader);
        match crate::apply::apply(
            &mut crate::store::LaneCtx {
                identity: &self.identity,
                source: &self.store,
                control: &mut self.writer,
                request: &control,
            },
            prepared,
        )? {
            crate::apply::Applied::Response(value) => Ok(value),
            crate::apply::Applied::Connected { response, .. } => crate::to_value(response),
        }
    }
}
