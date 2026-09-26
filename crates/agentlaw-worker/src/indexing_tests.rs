use crate::{derived::*, indexing::*, *};
struct Source {
    revision: String,
    count: usize,
}
impl PublishedSourcePort for Source {
    fn read_published_changes(
        &self,
        _: &str,
        b: &SourcePosition,
        _: u32,
    ) -> derived::Result<PublishedPage> {
        let end = SourcePosition {
            epoch: b.epoch.clone(),
            sequence: b.sequence + 1,
        };
        Ok(PublishedPage {
            basis: b.clone(),
            covered_through: end.clone(),
            source_position: end,
            coverage_complete: true,
            batches: vec![PublishedBatch {
                sequence: b.sequence + 1,
                documents: (0..self.count)
                    .map(|n| DerivedDocument {
                        memory_id: format!("id-{n}"),
                        change_id: self.revision.clone(),
                        scope: "user".into(),
                        body: Some(format!("published {}", self.revision)),
                        section: "body".into(),
                    })
                    .collect(),
            }],
        })
    }
}
fn context() -> DerivedContext {
    DerivedContext {
        repository_id: "r".into(),
        model_digest: "m".into(),
        config_digest: "c".into(),
        initial_basis: SourcePosition {
            epoch: "e".into(),
            sequence: 0,
        },
    }
}
fn fixture() -> (std::path::PathBuf, Broker) {
    let dir = std::env::temp_dir().join(format!("agentlaw-ack-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let b = Broker::open(dir.join("broker.sqlite"), 1).unwrap();
    (dir, b)
}
#[test]
fn bootstrap_zero_fence_is_durable_and_never_empty_complete() {
    struct Inventory(PublishedPage);
    impl PublishedSourcePort for Inventory {
        fn read_published_changes(
            &self,
            _: &str,
            _: &SourcePosition,
            _: u32,
        ) -> derived::Result<PublishedPage> {
            Ok(self.0.clone())
        }
    }
    let (dir, mut b) = fixture();
    let c = context();
    let inventory = |id: &str| {
        Inventory(PublishedPage {
            basis: c.initial_basis.clone(),
            covered_through: c.initial_basis.clone(),
            source_position: c.initial_basis.clone(),
            coverage_complete: true,
            batches: vec![PublishedBatch {
                sequence: 0,
                documents: vec![DerivedDocument {
                    memory_id: id.into(),
                    change_id: id.into(),
                    scope: "user".into(),
                    body: Some(format!("bootstrap {id}")),
                    section: "body".into(),
                }],
            }],
        })
    };
    DerivedWorkCoordinator::new(&mut b, &inventory("first"))
        .unwrap()
        .bootstrap_with_spools(&c, 0, false, &dir, &[])
        .unwrap();
    assert!(!bootstrap_channel_complete(&b, &c, Channel::Lexical).unwrap());
    assert!(!flush_channel(&mut b, &c, &dir, Channel::Lexical).unwrap());
    drop(b);
    let mut b = Broker::open(dir.join("broker.sqlite"), 1).unwrap();
    // Lost reply replay is idempotent, and a missing page cannot finalize coverage.
    DerivedWorkCoordinator::new(&mut b, &inventory("first"))
        .unwrap()
        .bootstrap_with_spools(&c, 0, false, &dir, &[])
        .unwrap();
    assert!(DerivedWorkCoordinator::new(&mut b, &inventory("second"))
        .unwrap()
        .bootstrap_with_spools(&c, 2, true, &dir, &[])
        .is_err());
    DerivedWorkCoordinator::new(&mut b, &inventory("second"))
        .unwrap()
        .bootstrap_with_spools(&c, 1, true, &dir, &[])
        .unwrap();
    assert!(flush_channel(&mut b, &c, &dir, Channel::Lexical).unwrap());
    assert!(!flush_channel(&mut b, &c, &dir, Channel::Vector).unwrap());
    let index =
        agentlaw_search::SearchIndex::open(directory(&dir, &c).unwrap().join("lexical.sqlite"))
            .unwrap();
    assert_eq!(
        index
            .read_view()
            .unwrap()
            .search(
                "bootstrap",
                &agentlaw_search::ScopeFilter {
                    allowed_scopes: vec!["user".into()]
                },
                10,
                &[],
                &[],
                true
            )
            .unwrap()
            .len(),
        2
    );
    drop(index);
    let mut vector = vec![0f32; 256];
    vector[0] = 1.;
    let bytes: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
    b.connection
        .execute("UPDATE jobs SET state='ready_to_index',result=?1", [bytes])
        .unwrap();
    assert!(flush_channel(&mut b, &c, &dir, Channel::Vector).unwrap());
    assert!(bootstrap_channel_complete(&b, &c, Channel::Vector).unwrap());
    assert_eq!(
        channel_position(&b, &c, Channel::Vector).unwrap().sequence,
        0
    );
    let repaired = repair_index(&mut b, &c, &dir).unwrap();
    assert_eq!(repaired.vector.sequence, 0);
    let index = agentlaw_search::ExactVectorIndex::open(
        directory(&dir, &c).unwrap().join("vector.sqlite"),
        "m",
        "c",
        256,
    )
    .unwrap();
    assert_eq!(
        index
            .read_view()
            .unwrap()
            .search_ann(
                &vector,
                &agentlaw_search::ScopeFilter {
                    allowed_scopes: vec!["user".into()]
                },
                10
            )
            .unwrap()
            .0
            .len(),
        2
    );
    drop(index);
    drop(b);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn independent_ack_missing_ledger_and_foreground_admission() {
    let (dir, mut b) = fixture();
    let c = context();
    DerivedWorkCoordinator::new(
        &mut b,
        &Source {
            revision: "v1".into(),
            count: 70,
        },
    )
    .unwrap()
    .advance(&c, 1)
    .unwrap();
    let key = JobKey {
        model_digest: "m".into(),
        config_digest: "query".into(),
        memory_id: "query".into(),
        change_id: "q".into(),
        content_digest: "q".into(),
        section: "q".into(),
    };
    assert!(b.enqueue(&key, "query", false, true, None).is_ok());
    assert!(flush_channel(&mut b, &c, &dir, Channel::Lexical).unwrap());
    assert_eq!(
        channel_position(&b, &c, Channel::Lexical).unwrap().sequence,
        1
    );
    assert_eq!(
        channel_position(&b, &c, Channel::Vector).unwrap().sequence,
        0
    );
    assert!(!flush_channel(&mut b, &c, &dir, Channel::Vector).unwrap());
    b.connection
        .execute("DELETE FROM derived_publications", [])
        .unwrap();
    assert!(ready_index_batch(&mut b, &c, Channel::Vector, 8).is_err());
    drop(b);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn superseded_failed_revision_does_not_block_current_vector() {
    let (dir, mut b) = fixture();
    let c = context();
    DerivedWorkCoordinator::new(
        &mut b,
        &Source {
            revision: "old".into(),
            count: 1,
        },
    )
    .unwrap()
    .advance(&c, 1)
    .unwrap();
    b.connection
        .execute("UPDATE jobs SET state='failed',error='old failure'", [])
        .unwrap();
    DerivedWorkCoordinator::new(
        &mut b,
        &Source {
            revision: "new".into(),
            count: 1,
        },
    )
    .unwrap()
    .advance(&c, 1)
    .unwrap();
    let mut fixture_vector = vec![0f32; 256];
    fixture_vector[0] = 1.0;
    let bytes: Vec<u8> = fixture_vector
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    b.connection
        .execute(
            "UPDATE jobs SET state='ready_to_index',result=?1 WHERE change_id='new'",
            [bytes],
        )
        .unwrap();
    assert!(flush_channel(&mut b, &c, &dir, Channel::Lexical).unwrap());
    assert!(flush_channel(&mut b, &c, &dir, Channel::Vector).unwrap());
    assert_eq!(
        channel_position(&b, &c, Channel::Vector).unwrap().sequence,
        2
    );
    let index = agentlaw_search::ExactVectorIndex::open(
        directory(&dir, &c).unwrap().join("vector.sqlite"),
        "m",
        "c",
        256,
    )
    .unwrap();
    let view = index.read_view().unwrap();
    let (hits, _) = view
        .search_ann(
            &fixture_vector,
            &agentlaw_search::ScopeFilter {
                allowed_scopes: vec!["user".into()],
            },
            8,
        )
        .unwrap();
    assert_eq!(hits[0].change_id, "new");
    drop(view);
    drop(index);
    drop(b);
    std::fs::remove_dir_all(dir).unwrap();
}
