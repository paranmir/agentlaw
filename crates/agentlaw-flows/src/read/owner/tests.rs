use super::*;
fn hit(id: &str, score: f64) -> Hit {
    Hit {
        memory_id: id.into(),
        change_id: "version".into(),
        score,
    }
}
#[test]
fn full_selection_accepts_a_single_clear_channel_but_not_conflicting_winners() {
    assert_eq!(
        uncontested(&[hit("a", 0.96), hit("b", 0.89)], &[]).as_deref(),
        Some("a")
    );
    assert_eq!(uncontested(&[], &[hit("a", 0.95)]).as_deref(), Some("a"));
    assert_eq!(uncontested(&[hit("a", 0.96)], &[hit("b", 0.96)]), None);
    assert_eq!(uncontested(&[hit("a", 0.94), hit("b", 0.92)], &[]), None);
}

use crate::semantic::{
    HistoryIngestHint, HistorySemanticPort, OfferResult, PiecePlan, PieceVectorReuse,
    PrefixEmbedding, SemanticCandidates, SemanticPort,
};
use agentlaw_storage::{Mutation, Store};
use std::sync::{Arc, Mutex};

struct SearchFixture {
    _dir: tempfile::TempDir,
    store: Store,
    registry: crate::exact::ProjectionGenerations,
    identity: StoreIdentity,
}

impl SearchFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        let root = dir.path().join("source");
        let store = Store::open(&root, &local).unwrap();
        let source = store.owned_published_reader();
        let initial = source.read_snapshot().unwrap().position().clone();
        let exact = local.join("exact-fixture.sqlite");
        let lexical = local.join("lexical.sqlite");
        drop(crate::exact::ExactIndex::open(&exact).unwrap());
        drop(agentlaw_search::SearchIndex::open(&lexical).unwrap());
        let db = rusqlite::Connection::open(&exact).unwrap();
        db.execute(
            "INSERT INTO exact_position VALUES(1,?1,?2)",
            rusqlite::params![initial.epoch, initial.sequence.to_string()],
        )
        .unwrap();
        db.execute("INSERT INTO exact_lexical VALUES(1,'lexical.sqlite')", [])
            .unwrap();
        drop(db);
        let registry =
            crate::exact::ProjectionGenerations::open("fixture", &initial.epoch, exact, lexical)
                .unwrap();
        drop(crate::control::ControlWriter::open(&local).unwrap());
        let identity = StoreIdentity {
            root,
            local,
            user_id: "user".into(),
            machine_id: uuid::Uuid::new_v4().to_string(),
            binding_id: "binding".into(),
            history_limit: 65_536,
            response_limit: 65_536,
            config_file: None,
        };
        Self {
            _dir: dir,
            store,
            registry,
            identity,
        }
    }

    fn publish(&self, body: &str, procedure: bool, private: bool) -> Hit {
        let id = uuid::Uuid::new_v4().to_string();
        let change = uuid::Uuid::new_v4().to_string();
        let scope = if private {
            json!({"scope":"machine", "machine_id":uuid::Uuid::new_v4().to_string()})
        } else {
            json!({"scope":"user"})
        };
        let kind = if procedure {
            "learned_procedure"
        } else {
            "memory"
        };
        let mut metadata = json!({
            "change_id":change, "applicability":scope,
            "origin":{"machine_id":self.identity.machine_id,"user_id":"user"},
            "recorded_at_ms":0, "is_rule":false, "relations":[], "work_targets":[]
        });
        if procedure {
            metadata["name"] = json!("Procedure");
            metadata["use_when"] = json!(body);
            metadata["evidence_memory_ids"] = json!([]);
        }
        self.store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![Mutation {
                    unit: CurrentUnit {
                        entity_id: id.clone(),
                        entity_type: kind.into(),
                        state: UnitState::Live {
                            heads: vec![Head {
                                metadata,
                                body: body.into(),
                            }],
                        },
                    },
                    expected_versions: vec![],
                    evidence: "published fixture".into(),
                }],
            )
            .unwrap();
        Hit {
            memory_id: id,
            change_id: change,
            score: 0.0,
        }
    }

    fn read<T>(&self, model: &Model, run: impl FnOnce(&ReadCtx<'_>, &mut ExactSnapshot) -> T) -> T {
        let source = self.store.owned_published_reader();
        let reader = crate::control::ControlReader::open(&self.identity.local).unwrap();
        let cache = crate::history_cache::HistoryCacheGuards::new();
        let request = RequestControl::default();
        let ctx = ReadCtx {
            identity: &self.identity,
            source: &source,
            control: &reader,
            semantic: model,
            history_semantic: model,
            history_cache: &cache,
            request: &request,
        };
        let guard = source.read_snapshot().unwrap();
        let mut exact =
            ExactSnapshot::build(self.registry.pin().unwrap(), &guard, DeltaBudget::default())
                .unwrap();
        drop(guard);
        run(&ctx, &mut exact)
    }
}

#[derive(Clone, Copy)]
enum Coverage {
    Complete,
    Partial,
    Loading,
}

struct Model {
    coverage: Coverage,
    prefix_inputs: Mutex<Vec<String>>,
    limits: Mutex<Vec<usize>>,
    hits: [Vec<Hit>; 3],
    piece_failure: bool,
}
impl Model {
    fn new(coverage: Coverage) -> Self {
        Self {
            coverage,
            prefix_inputs: Mutex::default(),
            limits: Mutex::default(),
            hits: Default::default(),
            piece_failure: false,
        }
    }
    fn vector(marker: f32) -> QueryVector {
        QueryVector {
            model: "model".into(),
            values: Arc::from([marker]),
        }
    }
}
impl SemanticPort for Model {
    fn embed_query(
        &self,
        _: &str,
        _: &RequestControl,
    ) -> std::result::Result<QueryVector, SemanticError> {
        if matches!(self.coverage, Coverage::Loading) {
            Err(SemanticError::Unavailable("model loading".into()))
        } else {
            Ok(Self::vector(1.0))
        }
    }
    fn embed_prefix(
        &self,
        text: &str,
        control: &RequestControl,
    ) -> std::result::Result<PrefixEmbedding, SemanticError> {
        self.prefix_inputs.lock().unwrap().push(text.into());
        Ok(PrefixEmbedding {
            vector: self.embed_query(text, control)?,
            truncated: true,
        })
    }
    fn plan_pieces(
        &self,
        _: &[&str],
        _: &RequestControl,
    ) -> std::result::Result<PiecePlan, SemanticError> {
        Ok(PiecePlan::default())
    }
    fn embed_planned(
        &self,
        _: &PiecePlan,
        _: usize,
        _: &PieceVectorReuse,
        _: &RequestControl,
    ) -> std::result::Result<Vec<QueryVector>, SemanticError> {
        if self.piece_failure {
            Err(SemanticError::Failed(
                "second piece inference failed".into(),
            ))
        } else {
            Ok((1..=3).map(|i| Self::vector(i as f32)).collect())
        }
    }
    fn search_available(
        &self,
        query: SemanticQuery<'_>,
        _: &RequestControl,
    ) -> std::result::Result<SemanticCandidates, SemanticError> {
        self.limits.lock().unwrap().push(query.limit);
        let coverage = match self.coverage {
            Coverage::Complete => SemanticCoverage::CompleteAt(query.source.clone()),
            Coverage::Partial => SemanticCoverage::Partial {
                available: agentlaw_storage::published::SourcePosition {
                    epoch: query.source.epoch.clone(),
                    sequence: 0,
                },
                requested: query.source.clone(),
            },
            Coverage::Loading => unreachable!("missing query vector"),
        };
        let hits = self.hits[query.vector.unwrap().values[0] as usize - 1].clone();
        Ok(SemanticCandidates { hits, coverage })
    }
}
impl HistorySemanticPort for Model {
    fn offer(&self, _: HistoryIngestHint) -> OfferResult {
        unreachable!()
    }
    fn search_available(
        &self,
        _: &str,
        _: &str,
        _: &QueryVector,
        _: &[String],
        _: usize,
        _: &RequestControl,
    ) -> std::result::Result<SemanticCandidates, SemanticError> {
        unreachable!()
    }
}

fn truncation_case(coverage: Coverage, procedures: bool) {
    let f = SearchFixture::new();
    let target = f.publish("zebralexicaltail", procedures, false);
    let model = Model::new(coverage);
    let long = format!("{}zebralexicaltail", "irrelevantword ".repeat(599));
    let value = f.read(&model, |ctx, exact| {
        if procedures {
            search_procedures_filtered(ctx, exact, &long, Some("user"), None, None, 10).unwrap()
        } else {
            let request = serde_json::from_value(json!({
                "recall_for":long, "search_terms":["another search perspective"],
                "restore_context":true, "memory_candidate_limit":10,
            }))
            .unwrap();
            recall(ctx, exact, &request).unwrap()
        }
    });
    let found = if procedures {
        value["procedures"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|item| item["procedure_id"] == target.memory_id)
    } else {
        ["candidates", "memories"].iter().any(|key| {
            value[*key]
                .as_array()
                .into_iter()
                .flatten()
                .any(|item| item["memory_id"] == target.memory_id)
        })
    };
    let count = |code: &str| {
        value["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|diagnostic| diagnostic["code"] == code)
            .count()
    };
    let expected = match coverage {
        Coverage::Complete => (1, false),
        Coverage::Partial => (1, true),
        Coverage::Loading => (0, true),
    };
    let inputs = model.prefix_inputs.lock().unwrap();
    assert_eq!(
        (
            count("semantic_query_truncated"),
            count("semantic_channel_incomplete") > 0,
        ),
        expected
    );
    assert!(
        found,
        "lexical search missed the word after the token limit: {value}"
    );
    assert!(inputs
        .iter()
        .any(|input| input.contains("zebralexicaltail")));
    if !procedures && !matches!(coverage, Coverage::Loading) {
        assert!(
            inputs.len() > 1,
            "restore must exercise multiple prefix requests"
        );
    }
}

#[test]
fn recall_reports_truncation_once_across_restore_views() {
    truncation_case(Coverage::Complete, false);
}
#[test]
fn recall_reports_truncation_with_partial_coverage() {
    truncation_case(Coverage::Partial, false);
}
#[test]
fn recall_loading_model_reports_only_incompleteness() {
    truncation_case(Coverage::Loading, false);
}
#[test]
fn procedure_query_reports_truncation_with_complete_coverage() {
    truncation_case(Coverage::Complete, true);
}
#[test]
fn procedure_query_reports_truncation_with_partial_coverage() {
    truncation_case(Coverage::Partial, true);
}
#[test]
fn procedure_query_loading_model_reports_only_incompleteness() {
    truncation_case(Coverage::Loading, true);
}

fn score(mut hit: Hit, score: f64) -> Hit {
    hit.score = score;
    hit
}

fn planned(f: &SearchFixture, model: &Model, query: &str) -> Channels {
    f.read(model, |ctx, exact| {
        let plan = model.plan_pieces(&[query], ctx.request).unwrap();
        preparation_channels_planned(
            ctx,
            exact,
            query,
            &["user".into()],
            &plan,
            0,
            &PieceVectorReuse::default(),
        )
        .unwrap()
    })
}

#[test]
fn piece_maxima_admit_the_matching_clear_lexical_winner() {
    let f = SearchFixture::new();
    let a = f.publish("harbor shipping container inspection", false, false);
    let b = f.publish("orchard pruning", false, false);
    let mut model = Model::new(Coverage::Complete);
    model.hits = [
        vec![score(a.clone(), 0.95)],
        vec![score(a.clone(), 0.7)],
        vec![score(b.clone(), 0.93)],
    ];
    let channels = planned(&f, &model, "harbor shipping container inspection");
    assert!(channels.complete);
    assert_eq!(channels.vector, [score(a.clone(), 0.95), score(b, 0.93)]);
    assert_eq!(uncontested(&[], &channels.vector), None);
    assert_eq!(
        uncontested(&channels.lexical, &channels.vector),
        Some(a.memory_id)
    );
}

#[test]
fn two_supporting_channels_do_not_admit_a_candidate() {
    let f = SearchFixture::new();
    let a = f.publish("harbor shipping", false, false);
    let b = f.publish("harbor shipping", false, false);
    let mut model = Model::new(Coverage::Complete);
    model.hits = [
        vec![score(a.clone(), 0.95)],
        vec![],
        vec![score(b.clone(), 0.93)],
    ];
    let channels = planned(&f, &model, "harbor shipping");
    assert!(channels.complete);
    assert_eq!(channels.vector, [score(a, 0.95), score(b, 0.93)]);
    assert_eq!(uncontested(&channels.lexical, &channels.vector), None);
}

#[test]
fn equal_piece_maxima_have_no_vector_winner() {
    let f = SearchFixture::new();
    let a = f.publish("harbor", false, false);
    let b = f.publish("orchard", false, false);
    let mut model = Model::new(Coverage::Complete);
    model.hits = [vec![score(a, 0.95)], vec![score(b, 0.95)], vec![]];
    let channels = planned(&f, &model, "harbor");
    assert_eq!(
        channels
            .vector
            .iter()
            .map(|hit| hit.score)
            .collect::<Vec<_>>(),
        [0.95, 0.95]
    );
    assert_eq!(uncontested(&[], &channels.vector), None);
}

#[test]
fn failed_piece_marks_the_vector_channel_incomplete() {
    let f = SearchFixture::new();
    let mut model = Model::new(Coverage::Complete);
    model.piece_failure = true;
    let channels = planned(&f, &model, "harbor");
    assert!(!channels.complete);
    assert!(channels.vector.is_empty());
    assert!(channels
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "semantic_channel_incomplete"));
    assert!(model.limits.lock().unwrap().is_empty());
}

#[test]
fn piece_search_preserves_backend_width_before_scope_filtering() {
    let f = SearchFixture::new();
    let hidden = f.publish("restricted", false, true);
    let visible = f.publish("public", false, false);
    let mut model = Model::new(Coverage::Complete);
    model.hits = [
        vec![score(hidden, 0.98), score(visible.clone(), 0.91)],
        vec![],
        vec![],
    ];
    let channels = planned(&f, &model, "public");
    assert_eq!(*model.limits.lock().unwrap(), [SEARCH_LIMIT; 3]);
    assert_eq!(channels.vector, [score(visible, 0.91)]);
}
