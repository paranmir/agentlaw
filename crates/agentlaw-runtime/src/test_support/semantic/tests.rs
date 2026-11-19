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
            fake.embed_prefix("", &control),
            Err(SemanticError::Unavailable(_))
        ));
        assert!(start.elapsed() < Duration::from_millis(100));
    }
    fake.set_model(FakeModel::Ready);
    fake.set_query_delay(Duration::from_millis(50));
    let start = Instant::now();
    fake.embed_prefix("", &control).unwrap().vector;
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
        fake.embed_prefix("", &control),
        Err(SemanticError::Unavailable("model loading".into()))
    );
    fake.set_model(FakeModel::Unavailable("offline".into()));
    assert_eq!(
        fake.embed_prefix("", &control),
        Err(SemanticError::Unavailable("offline".into()))
    );
    fake.set_model(FakeModel::Ready);
    let vector = fake.embed_prefix("", &control).unwrap().vector;
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
    let worker = std::thread::spawn(move || worker_fake.embed_prefix("", &worker_control));
    std::thread::sleep(Duration::from_millis(30));
    control
        .cancel
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(worker.join().unwrap(), Err(SemanticError::Cancelled));
    assert!(start.elapsed() < Duration::from_secs(2));
}
