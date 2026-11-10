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
mod tests;
