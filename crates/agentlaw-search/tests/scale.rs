use agentlaw_search::*;
fn temporary(label: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("agentlaw-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&p).unwrap();
    p
}
#[test]
fn ten_thousand_disk_vectors_incremental_ann_scope_and_snapshot() {
    let dir = temporary("ann-scale");
    let mut index = ExactVectorIndex::open(dir.join("v.sqlite"), "m", "c", 16).unwrap();
    let vector = |id: u64| -> Vec<f32> {
        (0u64..16)
            .map(|d| {
                let mut n = id
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(d.wrapping_mul(1442695040888963407));
                n ^= n >> 21;
                n ^= n << 13;
                n ^= n >> 7;
                (n as i32) as f32 / i32::MAX as f32
            })
            .collect()
    };
    for batch in 0..40 {
        let records: Vec<_> = (batch * 250..(batch + 1) * 250)
            .map(|id| VectorRecord {
                memory_id: format!("{id:05}"),
                change_id: "v1".into(),
                section: "body".into(),
                scope: if id % 2 == 0 { "user" } else { "private" }.into(),
                vector: vector(id),
            })
            .collect();
        index.commit(batch as i64 + 1, &records, &[]).unwrap();
    }
    let view = index.read_view().unwrap();
    let scope = ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    let (hits, metrics) = view.search_ann(&vector(8888), &scope, 10).unwrap();
    assert_eq!(hits[0].memory_id, "08888");
    assert!(metrics.candidates_scored < 5000);
    assert!(hits
        .iter()
        .all(|h| h.memory_id.parse::<u64>().unwrap() % 2 == 0));
    index.commit(41, &[], &["08888".into()]).unwrap();
    assert_eq!(
        view.search_ann(&vector(8888), &scope, 1).unwrap().0[0].memory_id,
        "08888"
    );
    assert!(index
        .read_view()
        .unwrap()
        .search_ann(&vector(8888), &scope, 10)
        .unwrap()
        .0
        .iter()
        .all(|h| h.memory_id != "08888"));
    drop(view);
    drop(index);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn scope_statistics_follow_only_changed_ids() {
    let dir = temporary("stats");
    let scope = ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    let mut cached = SearchIndex::open(dir.join("cached.sqlite")).unwrap();
    let mut baseline = SearchIndex::open(dir.join("baseline.sqlite")).unwrap();
    let doc = |id: &str, text: &str| SearchDocument {
        memory_id: id.into(),
        change_id: "v1".into(),
        scope: "user".into(),
        body: text.into(),
    };
    let docs = vec![
        doc("a", "alpha beta"),
        doc("b", "alpha gamma"),
        doc("c", "unrelated"),
    ];
    for i in [&mut cached, &mut baseline] {
        i.commit(1, &docs, &[]).unwrap();
    }
    cached.prepare_scope(&scope).unwrap();
    for i in [&mut cached, &mut baseline] {
        i.commit(2, &[doc("b", "gamma changed changed")], &["c".into()])
            .unwrap();
    }
    assert_eq!(
        cached
            .read_view()
            .unwrap()
            .search("alpha gamma", &scope, 10, &[], &[], true)
            .unwrap(),
        baseline
            .read_view()
            .unwrap()
            .search("alpha gamma", &scope, 10, &[], &[], true)
            .unwrap()
    );
    assert_eq!(
        cached
            .read_view()
            .unwrap()
            .lexical_strength("alpha absent", &scope, 10)
            .unwrap(),
        baseline
            .read_view()
            .unwrap()
            .lexical_strength("alpha absent", &scope, 10)
            .unwrap()
    );
    drop(cached);
    drop(baseline);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn lexical_dictionary_stores_digest_once_and_numeric_postings() {
    let dir = temporary("lexicon");
    let path = dir.join("lexical.sqlite");
    let mut index = SearchIndex::open(&path).unwrap();
    let changed = (0..5000).map(|id| Ok(format!("memory-{id}")));
    let documents = (0..5000).map(|id| {
        Ok(SearchDocument {
            memory_id: format!("memory-{id}"),
            change_id: "v1".into(),
            scope: "user".into(),
            body: "alpha beta gamma delta epsilon".into(),
        })
    });
    index.commit_stream(1, changed, documents).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    let dictionary: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM lexicon WHERE length(digest)=32",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let postings: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM postings WHERE typeof(term)='integer'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dictionary, 5);
    assert_eq!(postings, 25000);
    let mut view = index.read_view().unwrap();
    assert_eq!(
        view.lexical_matched(
            "alpha",
            &ScopeFilter {
                allowed_scopes: vec!["user".into()]
            }
        )
        .unwrap(),
        5000
    );
    drop(view);
    drop(db);
    drop(index);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn generation_swap_retains_cross_catalog_reader() {
    let dir = temporary("generations");
    let mut catalog = generations::GenerationCatalog::open(&dir).unwrap();
    for sequence in 1..=2 {
        let build = catalog.begin("model", "config").unwrap();
        let mut index = SearchIndex::open(build.directory.join("lexical.sqlite")).unwrap();
        index.commit(sequence, &[], &[]).unwrap();
        let stamp = index.read_view().unwrap().stamp;
        drop(index);
        catalog.publish(build, sequence, &stamp, None).unwrap();
        if sequence == 1 {
            break;
        }
    }
    let mut other = generations::GenerationCatalog::open(&dir).unwrap();
    let lease = other.acquire().unwrap().unwrap();
    let old = lease.directory.clone();
    let build = catalog.begin("model", "config").unwrap();
    let mut index = SearchIndex::open(build.directory.join("lexical.sqlite")).unwrap();
    index.commit(2, &[], &[]).unwrap();
    let stamp = index.read_view().unwrap().stamp;
    drop(index);
    catalog.publish(build, 2, &stamp, None).unwrap();
    assert_eq!(catalog.reclaim().unwrap(), 0);
    assert!(old.exists());
    drop(lease);
    assert_eq!(catalog.reclaim().unwrap(), 1);
    assert!(!old.exists());
    drop(other);
    drop(catalog);
    std::fs::remove_dir_all(dir).unwrap();
}
