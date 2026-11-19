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

#[derive(Debug, Clone, PartialEq)]
pub struct PrefixEmbedding {
    pub vector: QueryVector,
    pub truncated: bool,
}

#[derive(Default)]
pub struct PieceVectorReuse {
    vectors: std::sync::Mutex<std::collections::HashMap<(String, [u8; 32]), Arc<[f32]>>>,
}

impl PieceVectorReuse {
    pub fn get(&self, model: &str, digest: [u8; 32]) -> Option<Arc<[f32]>> {
        self.vectors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(model.to_owned(), digest))
            .cloned()
    }

    pub fn insert(&self, model: &str, digest: [u8; 32], vector: Arc<[f32]>) {
        self.vectors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((model.to_owned(), digest), vector);
    }
}

#[derive(Default)]
pub struct PiecePlan {
    model: String,
    records: Vec<PieceRecord>,
}

impl PiecePlan {
    pub fn new(model: String, records: Vec<PieceRecord>) -> Self {
        Self { model, records }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn record(&self, index: usize) -> Option<&PieceRecord> {
        self.records.get(index)
    }
}

pub struct PieceRecord {
    text: Arc<str>,
    pieces: Vec<(std::ops::Range<usize>, [u8; 32])>,
}

impl PieceRecord {
    pub fn new(text: Arc<str>, pieces: Vec<(std::ops::Range<usize>, [u8; 32])>) -> Self {
        Self { text, pieces }
    }

    pub fn pieces(&self) -> impl Iterator<Item = (Option<&str>, &[u8; 32])> {
        self.pieces
            .iter()
            .map(|(range, digest)| (self.text.get(range.clone()), digest))
    }
}

pub trait SemanticPort: Send + Sync {
    fn embed_prefix(
        &self,
        text: &str,
        control: &RequestControl,
    ) -> Result<PrefixEmbedding, SemanticError> {
        #[cfg(any(test, feature = "test-support"))]
        {
            self.embed_query(text, control)
                .map(|vector| PrefixEmbedding {
                    vector,
                    truncated: false,
                })
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            let _ = (text, control);
            Err(SemanticError::Unavailable(
                "query embedding unavailable".into(),
            ))
        }
    }
    fn plan_pieces(
        &self,
        _records: &[&str],
        _control: &RequestControl,
    ) -> Result<PiecePlan, SemanticError> {
        Err(SemanticError::Failed("piece planning unavailable".into()))
    }
    fn embed_planned(
        &self,
        _plan: &PiecePlan,
        _record: usize,
        _reuse: &PieceVectorReuse,
        _control: &RequestControl,
    ) -> Result<Vec<QueryVector>, SemanticError> {
        Err(SemanticError::Failed("piece embedding unavailable".into()))
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    fn embed_query(
        &self,
        _text: &str,
        _control: &RequestControl,
    ) -> Result<QueryVector, SemanticError> {
        Err(SemanticError::Unavailable(
            "semantic embedding unavailable".into(),
        ))
    }
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
mod tests;
