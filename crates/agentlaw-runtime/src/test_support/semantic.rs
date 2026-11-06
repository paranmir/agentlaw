use agentlaw_contracts::RequestControl;
use agentlaw_flows::semantic::{
    HistoryIngestHint, HistorySemanticPort, OfferResult, QueryVector, SemanticCandidates,
    SemanticCoverage, SemanticError, SemanticPort, SemanticQuery,
};
use agentlaw_storage::published::SourcePosition;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeModel {
    Ready,
    Loading,
    Unavailable(String),
}
#[derive(Debug, Clone, PartialEq)]
pub enum FakeIndex {
    Complete,
    Partial { available: SourcePosition },
    Busy,
    Failed(String),
    Unavailable(String),
}
struct SemanticState {
    model: FakeModel,
    index: FakeIndex,
    hits: Vec<agentlaw_search::Hit>,
    delay: Duration,
    offer_result: OfferResult,
    embeds: usize,
    searches: usize,
    offers: Vec<HistoryIngestHint>,
}
impl Default for SemanticState {
    fn default() -> Self {
        Self {
            model: FakeModel::Ready,
            index: FakeIndex::Complete,
            hits: vec![],
            delay: Duration::ZERO,
            offer_result: OfferResult::Accepted,
            embeds: 0,
            searches: 0,
            offers: vec![],
        }
    }
}
#[derive(Clone, Default)]
pub struct FakeSemantic {
    state: Arc<Mutex<SemanticState>>,
}
impl FakeSemantic {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_model(&self, model: FakeModel) {
        self.state.lock().unwrap().model = model;
    }
    pub fn set_index(&self, index: FakeIndex) {
        self.state.lock().unwrap().index = index;
    }
    pub fn set_hits(&self, hits: Vec<agentlaw_search::Hit>) {
        self.state.lock().unwrap().hits = hits;
    }
    pub fn set_query_delay(&self, delay: Duration) {
        self.state.lock().unwrap().delay = delay;
    }
    pub fn set_offer_result(&self, result: OfferResult) {
        self.state.lock().unwrap().offer_result = result;
    }
    pub fn embed_calls(&self) -> usize {
        self.state.lock().unwrap().embeds
    }
    pub fn search_calls(&self) -> usize {
        self.state.lock().unwrap().searches
    }
    pub fn offers(&self) -> Vec<HistoryIngestHint> {
        self.state.lock().unwrap().offers.clone()
    }
    fn search(
        &self,
        requested: SourcePosition,
        limit: usize,
        control: &RequestControl,
    ) -> Result<SemanticCandidates, SemanticError> {
        let (index, hits) = {
            let mut s = self.state.lock().unwrap();
            s.searches += 1;
            (s.index.clone(), s.hits.clone())
        };
        control.check().map_err(|_| SemanticError::Cancelled)?;
        let coverage = match index {
            FakeIndex::Busy => return Err(SemanticError::Busy),
            FakeIndex::Failed(e) => return Err(SemanticError::Failed(e)),
            FakeIndex::Unavailable(e) => {
                return Ok(SemanticCandidates {
                    hits: vec![],
                    coverage: SemanticCoverage::Unavailable(e),
                })
            }
            FakeIndex::Complete => SemanticCoverage::CompleteAt(requested),
            FakeIndex::Partial { available } => SemanticCoverage::Partial {
                available,
                requested,
            },
        };
        Ok(SemanticCandidates {
            hits: hits.into_iter().take(limit).collect(),
            coverage,
        })
    }
}
impl SemanticPort for FakeSemantic {
    fn embed_query(&self, _: &str, control: &RequestControl) -> Result<QueryVector, SemanticError> {
        let (model, delay) = {
            let mut s = self.state.lock().unwrap();
            s.embeds += 1;
            (s.model.clone(), s.delay)
        };
        control.check().map_err(|_| SemanticError::Cancelled)?;
        match model {
            FakeModel::Loading => return Err(SemanticError::Unavailable("model loading".into())),
            FakeModel::Unavailable(e) => return Err(SemanticError::Unavailable(e)),
            FakeModel::Ready => {}
        }
        let start = Instant::now();
        while start.elapsed() < delay {
            std::thread::sleep(Duration::from_millis(5).min(delay.saturating_sub(start.elapsed())));
            control.check().map_err(|_| SemanticError::Cancelled)?;
        }
        let mut values = vec![0.0; 256];
        values[0] = 1.0;
        Ok(QueryVector {
            model: "fake-model".into(),
            values: values.into(),
        })
    }
    fn search_available(
        &self,
        query: SemanticQuery<'_>,
        control: &RequestControl,
    ) -> Result<SemanticCandidates, SemanticError> {
        self.search(query.source.clone(), query.limit, control)
    }
}
impl HistorySemanticPort for FakeSemantic {
    fn offer(&self, hint: HistoryIngestHint) -> OfferResult {
        let mut s = self.state.lock().unwrap();
        s.offers.push(hint);
        s.offer_result
    }
    fn search_available(
        &self,
        _: &str,
        _: &str,
        _: &QueryVector,
        _: &[String],
        limit: usize,
        control: &RequestControl,
    ) -> Result<SemanticCandidates, SemanticError> {
        self.search(
            SourcePosition {
                epoch: "fake".into(),
                sequence: 0,
            },
            limit,
            control,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_models_do_not_wait_but_ready_does() {
        let fake = FakeSemantic::new();
        let control = RequestControl::default();
        fake.set_query_delay(Duration::from_secs(1));
        for model in [FakeModel::Loading, FakeModel::Unavailable("offline".into())] {
            fake.set_model(model);
            let start = Instant::now();
            assert!(matches!(
                fake.embed_query("", &control),
                Err(SemanticError::Unavailable(_))
            ));
            assert!(start.elapsed() < Duration::from_millis(100));
        }
        fake.set_model(FakeModel::Ready);
        fake.set_query_delay(Duration::from_millis(50));
        let start = Instant::now();
        fake.embed_query("", &control).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(50));
        // The existing cancellation test checks interruption of a delayed Ready query.
    }

    fn position(epoch: &str, sequence: u64) -> SourcePosition {
        SourcePosition {
            epoch: epoch.into(),
            sequence,
        }
    }

    #[test]
    fn semantic_states_and_history() {
        let fake = FakeSemantic::new();
        let control = RequestControl::default();
        fake.set_model(FakeModel::Loading);
        assert_eq!(
            fake.embed_query("", &control),
            Err(SemanticError::Unavailable("model loading".into()))
        );
        fake.set_model(FakeModel::Unavailable("offline".into()));
        assert_eq!(
            fake.embed_query("", &control),
            Err(SemanticError::Unavailable("offline".into()))
        );
        fake.set_model(FakeModel::Ready);
        let vector = fake.embed_query("", &control).unwrap();
        assert_eq!(vector.values.len(), 256);
        assert_eq!(vector.values[0], 1.0);
        let source = position("source", 5);
        let query = SemanticQuery {
            binding: "b",
            scopes: &[],
            text: "",
            vector: Some(&vector),
            limit: 0,
            source: &source,
        };
        fake.set_index(FakeIndex::Busy);
        assert_eq!(
            SemanticPort::search_available(&fake, query, &control),
            Err(SemanticError::Busy)
        );
        let available = position("old", 2);
        fake.set_index(FakeIndex::Partial {
            available: available.clone(),
        });
        let result = SemanticPort::search_available(&fake, query, &control).unwrap();
        assert_eq!(
            result.coverage,
            SemanticCoverage::Partial {
                available,
                requested: source.clone()
            }
        );
        assert!(result.hits.is_empty());
        let hint = HistoryIngestHint {
            binding: "b".into(),
            subject: "s".into(),
            through: source,
        };
        fake.set_offer_result(OfferResult::Queued);
        assert_eq!(fake.offer(hint.clone()), OfferResult::Queued);
        assert_eq!(fake.offers(), vec![hint]);
        assert_eq!(fake.embed_calls(), 3);
        assert_eq!(fake.search_calls(), 2);
    }

    #[test]
    fn semantic_cancellation_and_limit() {
        let fake = FakeSemantic::new();
        fake.set_hits(vec![
            agentlaw_search::Hit {
                change_id: "a".into(),
                memory_id: "a".into(),
                score: 1.0,
            },
            agentlaw_search::Hit {
                change_id: "b".into(),
                memory_id: "b".into(),
                score: 0.5,
            },
        ]);
        let control = RequestControl::default();
        let source = position("source", 1);
        let query = SemanticQuery {
            binding: "b",
            scopes: &[],
            text: "",
            vector: None,
            limit: 1,
            source: &source,
        };
        assert_eq!(
            SemanticPort::search_available(&fake, query, &control)
                .unwrap()
                .hits
                .len(),
            1
        );
        fake.set_query_delay(Duration::from_secs(10));
        let worker_fake = fake.clone();
        let worker_control = control.clone();
        let start = Instant::now();
        let worker = std::thread::spawn(move || worker_fake.embed_query("", &worker_control));
        std::thread::sleep(Duration::from_millis(30));
        control
            .cancel
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(worker.join().unwrap(), Err(SemanticError::Cancelled));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
