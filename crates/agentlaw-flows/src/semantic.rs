//! Ports for semantic search, implemented by the runtime.

use agentlaw_contracts::RequestControl;
use agentlaw_storage::published::SourcePosition;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct QueryVector {
    pub model: String,
    pub values: Arc<[f32]>,
}

#[derive(Debug, Clone, Copy)]
pub struct SemanticQuery<'a> {
    pub binding: &'a str,
    pub scopes: &'a [String],
    pub text: &'a str,
    pub vector: Option<&'a QueryVector>,
    pub limit: usize,
    pub source: &'a SourcePosition,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticCandidates {
    pub hits: Vec<agentlaw_search::Hit>,
    pub coverage: SemanticCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticCoverage {
    CompleteAt(SourcePosition),
    Partial {
        available: SourcePosition,
        requested: SourcePosition,
    },
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticError {
    Unavailable(String),
    Busy,
    Cancelled,
    Failed(String),
}

pub trait SemanticPort: Send + Sync {
    fn embed_query(
        &self,
        text: &str,
        control: &RequestControl,
    ) -> Result<QueryVector, SemanticError>;
    fn search_available(
        &self,
        query: SemanticQuery<'_>,
        control: &RequestControl,
    ) -> Result<SemanticCandidates, SemanticError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryIngestHint {
    pub binding: String,
    pub subject: String,
    pub through: SourcePosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferResult {
    Accepted,
    Queued,
    Busy,
}

pub trait HistorySemanticPort: Send + Sync {
    fn offer(&self, hint: HistoryIngestHint) -> OfferResult;
    fn search_available(
        &self,
        binding: &str,
        subject: &str,
        vector: &QueryVector,
        scopes: &[String],
        limit: usize,
        control: &RequestControl,
    ) -> Result<SemanticCandidates, SemanticError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ports;

    impl SemanticPort for Ports {
        fn embed_query(
            &self,
            _: &str,
            control: &RequestControl,
        ) -> Result<QueryVector, SemanticError> {
            control.check().map_err(|_| SemanticError::Cancelled)?;
            Ok(QueryVector {
                model: "test".into(),
                values: Arc::from([1.0]),
            })
        }

        fn search_available(
            &self,
            query: SemanticQuery<'_>,
            control: &RequestControl,
        ) -> Result<SemanticCandidates, SemanticError> {
            control.check().map_err(|_| SemanticError::Cancelled)?;
            Ok(SemanticCandidates {
                hits: vec![],
                coverage: SemanticCoverage::Partial {
                    available: query.source.clone(),
                    requested: query.source.clone(),
                },
            })
        }
    }

    impl HistorySemanticPort for Ports {
        fn offer(&self, _: HistoryIngestHint) -> OfferResult {
            OfferResult::Accepted
        }

        fn search_available(
            &self,
            _: &str,
            _: &str,
            _: &QueryVector,
            _: &[String],
            _: usize,
            control: &RequestControl,
        ) -> Result<SemanticCandidates, SemanticError> {
            control.check().map_err(|_| SemanticError::Cancelled)?;
            Ok(SemanticCandidates {
                hits: vec![],
                coverage: SemanticCoverage::Unavailable("test".into()),
            })
        }
    }

    #[test]
    fn ports_are_object_safe_and_usable_from_a_thread() {
        let semantic: Arc<dyn SemanticPort> = Arc::new(Ports);
        let history: Arc<dyn HistorySemanticPort> = Arc::new(Ports);
        std::thread::spawn(move || {
            let control = RequestControl::default();
            let source = SourcePosition {
                epoch: Default::default(),
                sequence: Default::default(),
            };
            let vector = semantic.embed_query("query", &control).unwrap();
            let query = SemanticQuery {
                binding: "binding",
                scopes: &[],
                text: "query",
                vector: Some(&vector),
                limit: 1,
                source: &source,
            };
            let result = semantic.search_available(query, &control).unwrap();
            assert_eq!(
                result.coverage,
                SemanticCoverage::Partial {
                    available: source.clone(),
                    requested: source.clone()
                }
            );
            assert_eq!(
                history.offer(HistoryIngestHint {
                    binding: "binding".into(),
                    subject: "subject".into(),
                    through: source
                }),
                OfferResult::Accepted
            );
            assert!(history
                .search_available("binding", "subject", &vector, &[], 1, &control)
                .is_ok());
        })
        .join()
        .unwrap();
    }
}
