mod effective;
mod open_existing;
use super::*;
fn doc(id: &str, body: &str, scope: &str) -> SearchDocument {
    SearchDocument {
        memory_id: id.into(),
        change_id: "v1".into(),
        body: body.into(),
        scope: scope.into(),
    }
}

#[test]
fn temporary_memory_index_supports_search_through_a_separate_view() {
    let uri = format!(
        "file:search-test-{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4()
    );
    let mut index = SearchIndex::open_temporary(uri).unwrap();
    assert_eq!(
        index
            .connection
            .query_row("PRAGMA temp_store", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    index
        .commit(1, &[doc("one", "cancellation exception", "history")], &[])
        .unwrap();
    let hits = index
        .read_view()
        .unwrap()
        .search(
            "cancellation",
            &ScopeFilter {
                allowed_scopes: vec!["history".into()],
            },
            5,
            &[],
            &[],
            true,
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].memory_id, "one");
}
#[test]
fn rrf_dedups_sections_and_ties() {
    let h = |id: &str, score| Hit {
        memory_id: id.into(),
        change_id: "v".into(),
        score,
    };
    let assert_scores = |actual: &[Hit], expected: &[(&str, f64)]| {
        assert_eq!(actual.len(), expected.len(), "unexpected fused identities");
        for (hit, (id, score)) in actual.iter().zip(expected) {
            assert_eq!(&hit.memory_id, id);
            assert!(hit.score.is_finite(), "{id} has a non-finite fused score");
            assert!(
                (hit.score - score).abs() < 1e-14,
                "{id}: expected {score:.17}, got {:.17}",
                hit.score
            );
        }
    };

    // Unsorted section hits. After per-memory max/dedup, the ranks are:
    // lexical: a, b, c, d; vector: b, a, e, d.
    // Duplicate a/b sections must neither vote again nor consume ranks.
    // Raw lexical magnitudes dwarf cosine scores but must not dominate fusion.
    let lexical = vec![
        h("d", 0.0),
        h("a", 0.5),
        h("b", 1e6),
        h("invalid-positive-infinity", f64::INFINITY),
        h("c", f64::NAN),
        h("a", 1e12),
        h("c", 1.0),
        h("a", 9e11),
    ];
    let vector = vec![
        h("e", 0.7),
        h("b", 0.1),
        h("d", -0.2),
        h("invalid-nan", f64::NAN),
        h("a", 0.8),
        h("b", 0.99),
        h("invalid-negative-infinity", f64::NEG_INFINITY),
        h("b", 0.98),
    ];
    let fused = reciprocal_rank_fusion(&[lexical.clone(), vector.clone()]);
    // Hand calculation for k=60, ranks starting at 1:
    // a,b = 1/61 + 1/62; d = 1/64 + 1/64; c,e = 1/63.
    // Thus two fourth places outrank either single third place; equal totals
    // are ordered by memory ID, not insertion order or raw score magnitude.
    assert_scores(
        &fused,
        &[
            ("a", 1.0 / 61.0 + 1.0 / 62.0),
            ("b", 1.0 / 61.0 + 1.0 / 62.0),
            ("d", 2.0 / 64.0),
            ("c", 1.0 / 63.0),
            ("e", 1.0 / 63.0),
        ],
    );

    let rescaled = reciprocal_rank_fusion(&[
        lexical
            .iter()
            .map(|hit| h(&hit.memory_id, hit.score * 1e-15))
            .collect(),
        vector
            .iter()
            .map(|hit| h(&hit.memory_id, hit.score * 1e9))
            .collect(),
    ]);
    assert_eq!(rescaled, fused, "fusion must depend on ranks, not units");
    let mut reversed_lexical = lexical.clone();
    let mut reversed_vector = vector.clone();
    reversed_lexical.reverse();
    reversed_vector.reverse();
    assert_eq!(
        reciprocal_rank_fusion(&[reversed_vector, reversed_lexical]),
        fused,
        "channel/section arrival order must not change tied fused results"
    );

    // An absent channel contributes nothing: no invented vote or averaging.
    let lexical_only = reciprocal_rank_fusion(&[lexical.clone(), vec![]]);
    assert_scores(
        &lexical_only,
        &[
            ("a", 1.0 / 61.0),
            ("b", 1.0 / 62.0),
            ("c", 1.0 / 63.0),
            ("d", 1.0 / 64.0),
        ],
    );
    assert_eq!(reciprocal_rank_fusion(&[vec![], lexical]), lexical_only);
    assert_scores(
        &reciprocal_rank_fusion(&[vec![], vector]),
        &[
            ("b", 1.0 / 61.0),
            ("a", 1.0 / 62.0),
            ("e", 1.0 / 63.0),
            ("d", 1.0 / 64.0),
        ],
    );
    assert!(reciprocal_rank_fusion(&[vec![], vec![]]).is_empty());
    assert!(reciprocal_rank_fusion(&[
        vec![h("nan", f64::NAN), h("infinity", f64::INFINITY)],
        vec![h("negative-infinity", f64::NEG_INFINITY)],
    ])
    .is_empty());

    // Equal raw scores are resolved before rank assignment, by stable ID.
    let tied = reciprocal_rank_fusion(&[
        vec![h("b", 1000.0), h("a", 1000.0)],
        vec![h("b", 0.5), h("a", 0.5)],
    ]);
    assert_scores(&tied, &[("a", 2.0 / 61.0), ("b", 2.0 / 62.0)]);
    assert_eq!(
        reciprocal_rank_fusion(&[
            vec![h("a", 1000.0), h("b", 1000.0)],
            vec![h("a", 0.5), h("b", 0.5)],
        ]),
        tied
    );
}
#[test]
fn snapshot_overlay_scope() {
    let p = std::env::temp_dir().join(format!(
        "agentlaw-search-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut index = SearchIndex::open(&p).unwrap();
    index
        .commit(
            1,
            &[
                doc("a", "old needle", "user"),
                doc("b", "needle", "private"),
            ],
            &[],
        )
        .unwrap();
    let mut old = index.read_view().unwrap();
    index
        .commit(2, &[doc("a", "replacement", "user")], &[])
        .unwrap();
    let scope = ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    assert_eq!(old.stamp.ack, 1);
    assert_eq!(
        old.search("needle", &scope, 10, &[], &[], true)
            .unwrap()
            .len(),
        1
    );
    assert!(old
        .search(
            "needle",
            &scope,
            10,
            &[doc("a", "replacement", "user")],
            &[],
            true
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        old.search("fresh", &scope, 10, &[doc("c", "fresh", "user")], &[], true)
            .unwrap()[0]
            .memory_id,
        "c"
    );
    assert!(old.search("x", &scope, 10, &[], &[], false).is_err());
    drop(old);
    drop(index);
    let _ = std::fs::remove_file(p);
}
#[test]
fn vector_validation() {
    assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0]).unwrap(), 1.0);
    assert!(cosine(&[0.0], &[0.0]).is_err());
}
#[test]
fn multihead_scope_does_not_leak_or_duplicate_identity() {
    let p = std::env::temp_dir().join(format!(
        "agentlaw-heads-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut index = SearchIndex::open(&p).unwrap();
    let first = doc("a", "public needle", "user");
    let mut second = doc("a", "private secret needle", "private");
    second.change_id = "v2".into();
    index.commit(1, &[first, second], &[]).unwrap();
    let mut view = index.read_view().unwrap();
    let user = ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    assert!(view
        .search("secret", &user, 10, &[], &[], true)
        .unwrap()
        .is_empty());
    let both = ScopeFilter {
        allowed_scopes: vec!["user".into(), "private".into()],
    };
    assert_eq!(
        view.search("needle", &both, 10, &[], &[], true)
            .unwrap()
            .len(),
        1
    );
    drop(view);
    drop(index);
    let _ = std::fs::remove_file(p);
}
#[test]
fn vector_disk_max_sections_overlay_and_snapshot() {
    let p = std::env::temp_dir().join(format!(
        "agentlaw-vector-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let rec = |id: &str, section: &str, vector| VectorRecord {
        memory_id: id.into(),
        change_id: "v1".into(),
        section: section.into(),
        scope: "user".into(),
        vector,
    };
    let mut index = ExactVectorIndex::open(&p, "model", "config", 2).unwrap();
    index
        .commit(
            1,
            &[
                rec("a", "one", vec![1.0, 0.0]),
                rec("a", "two", vec![0.0, 1.0]),
                rec("b", "one", vec![0.8, 0.2]),
            ],
            &[],
        )
        .unwrap();
    let old = index.read_view().unwrap();
    let scope = ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    let hits = old.search(&[1.0, 0.0], &scope, 2).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].memory_id, "a");
    assert_eq!(hits[0].score, 1.0);
    index.commit(2, &[], &["a".into()]).unwrap();
    assert_eq!(old.stamp.ack, 1);
    assert_eq!(
        old.search(&[1.0, 0.0], &scope, 2).unwrap()[0].memory_id,
        "a"
    );
    let hits = old
        .search_with_overlay(
            &[1.0, 0.0],
            &scope,
            2,
            &[rec("c", "no lexical match required", vec![1.0, 0.0])],
            &["a".into()],
            true,
        )
        .unwrap();
    assert_eq!(hits[0].memory_id, "c");
    assert!(!hits.iter().any(|h| h.memory_id == "a"));
    assert!(ExactVectorIndex::open(&p, "different model", "config", 2).is_err());
    drop(old);
    drop(index);
    let _ = std::fs::remove_file(p);
}
