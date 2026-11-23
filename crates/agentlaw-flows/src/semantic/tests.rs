use super::*;

struct Ports;

impl SemanticPort for Ports {
    fn embed_prefix(
        &self,
        _: &str,
        control: &RequestControl,
    ) -> Result<PrefixEmbedding, SemanticError> {
        control.check().map_err(|_| SemanticError::Cancelled)?;
        Ok(crate::semantic::PrefixEmbedding {
            vector: QueryVector {
                model: "test".into(),
                values: Arc::from([1.0]),
            },
            truncated: false,
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

    fn plan_pieces(
        &self,
        _: &[&str],
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::PiecePlan, crate::semantic::SemanticError> {
        Err(crate::semantic::SemanticError::Unavailable(
            "fixture has no piece planner".into(),
        ))
    }
    fn embed_planned(
        &self,
        _: &crate::semantic::PiecePlan,
        _: usize,
        _: &crate::semantic::PieceVectorReuse,
        _: &RequestControl,
    ) -> std::result::Result<Vec<crate::semantic::QueryVector>, crate::semantic::SemanticError>
    {
        Err(crate::semantic::SemanticError::Unavailable(
            "fixture has no piece embeddings".into(),
        ))
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
        let vector = semantic.embed_prefix("query", &control).unwrap().vector;
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
