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
    _private: (),
}

#[derive(Default)]
pub struct PiecePlan {
    _private: (),
}

pub trait SemanticPort: Send + Sync {
    fn embed_prefix(
        &self,
        _text: &str,
        _control: &RequestControl,
    ) -> Result<PrefixEmbedding, SemanticError> {
        Err(SemanticError::Failed(
            "prefix embedding is not implemented".into(),
        ))
    }
    fn plan_pieces(
        &self,
        _records: &[&str],
        _control: &RequestControl,
    ) -> Result<PiecePlan, SemanticError> {
        Err(SemanticError::Failed(
            "piece planning is not implemented".into(),
        ))
    }
    fn embed_planned(
        &self,
        _plan: &PiecePlan,
        _record: usize,
        _reuse: &PieceVectorReuse,
        _control: &RequestControl,
    ) -> Result<Vec<QueryVector>, SemanticError> {
        Err(SemanticError::Failed(
            "piece embedding is not implemented".into(),
        ))
    }

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
mod tests;
