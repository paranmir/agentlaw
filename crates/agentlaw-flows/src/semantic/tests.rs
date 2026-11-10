use super::*;

struct Ports;

impl SemanticPort for Ports {
    fn embed_query(&self, _: &str, control: &RequestControl) -> Result<QueryVector, SemanticError> {
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
