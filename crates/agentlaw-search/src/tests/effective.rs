use super::*;
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("agentlaw-effective-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn db(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn doc(id: &str, change: &str, body: &str, scope: &str) -> SearchDocument {
    SearchDocument {
        memory_id: id.into(),
        change_id: change.into(),
        body: body.into(),
        scope: scope.into(),
    }
}
fn scope() -> ScopeFilter {
    ScopeFilter {
        allowed_scopes: vec!["user".into()],
    }
}
fn empty() -> LexicalDelta<'static> {
    LexicalDelta {
        pending: &[],
        removed: &[],
        complete: true,
    }
}

#[test]
fn effective_matches_rebuilt_corpus_and_statistics() {
    let f = Fixture::new();
    let mut index = SearchIndex::open(f.db("old.db")).unwrap();
    let initial = vec![
        doc("a", "1", "alpha alpha beta", "user"),
        doc("b", "1", "beta", "user"),
        doc("c", "1", "alpha", "user"),
        doc("outside", "1", "alpha", "other"),
    ];
    index.commit(1, &initial, &[]).unwrap();
    index.prepare_scope(&scope()).unwrap();
    let pending = vec![
        doc("a", "2", "beta beta beta", "user"),
        doc("a", "3", "alpha alpha", "user"),
        doc("new", "1", "alpha filler filler filler filler", "user"),
        doc("outside", "2", "beta", "other"),
    ];
    let removed = vec!["a".into(), "c".into(), "outside".into()];
    let mut rebuilt = SearchIndex::open(f.db("new.db")).unwrap();
    let mut current = vec![initial[1].clone()];
    current.extend(pending.clone());
    rebuilt.commit(1, &current, &[]).unwrap();
    rebuilt.prepare_scope(&scope()).unwrap();
    let mut old_view = ReadView::open(&f.db("old.db")).unwrap();
    let mut reference = rebuilt.read_view().unwrap();
    let mut effective = old_view
        .effective(
            &scope(),
            LexicalDelta {
                pending: &pending,
                removed: &removed,
                complete: true,
            },
        )
        .unwrap();
    for query in ["alpha beta", "alpha", "beta missing"] {
        assert_eq!(
            effective.search(query, 20).unwrap(),
            reference
                .search(query, &scope(), 20, &[], &[], true)
                .unwrap()
        );
        assert_eq!(
            effective.strength(query, 20).unwrap(),
            reference.lexical_strength(query, &scope(), 20).unwrap()
        );
        assert_eq!(
            effective.matched(query).unwrap(),
            reference.lexical_matched(query, &scope()).unwrap()
        );
    }
}

#[test]
fn empty_delta_matches_legacy() {
    let f = Fixture::new();
    let path = f.db("index.db");
    let mut index = SearchIndex::open(&path).unwrap();
    index
        .commit(
            1,
            &[
                doc("a", "1", "alpha beta", "user"),
                doc("b", "1", "beta", "user"),
            ],
            &[],
        )
        .unwrap();
    index.prepare_scope(&scope()).unwrap();
    let mut old = index.read_view().unwrap();
    let mut view = ReadView::open(&path).unwrap();
    let mut corpus = view.effective(&scope(), empty()).unwrap();
    assert_eq!(
        corpus.search("alpha beta", 5).unwrap(),
        old.search("alpha beta", &scope(), 5, &[], &[], true)
            .unwrap()
    );
    assert_eq!(
        corpus.strength("alpha beta", 5).unwrap(),
        old.lexical_strength("alpha beta", &scope(), 5).unwrap()
    );
    assert_eq!(
        corpus.matched("alpha beta").unwrap(),
        old.lexical_matched("alpha beta", &scope()).unwrap()
    );
}

#[test]
fn changed_corpus_statistics_change_ranking() {
    let f = Fixture::new();
    let path = f.db("rank.db");
    let mut index = SearchIndex::open(&path).unwrap();
    let mut docs = vec![
        doc("a", "1", "alpha", "user"),
        doc("b", "1", "beta", "user"),
    ];
    for n in 0..5 {
        docs.push(doc(
            &format!("x{n}"),
            "1",
            "alpha padding padding padding",
            "user",
        ));
    }
    index.commit(1, &docs, &[]).unwrap();
    index.prepare_scope(&scope()).unwrap();
    let mut view = ReadView::open(&path).unwrap();
    let before = view
        .effective(&scope(), empty())
        .unwrap()
        .search("alpha beta", 10)
        .unwrap();
    assert_eq!(before[0].memory_id, "b");
    let removed: Vec<_> = (0..5).map(|n| format!("x{n}")).collect();
    let pending: Vec<_> = (0..5)
        .map(|n| {
            doc(
                &format!("y{n}"),
                "1",
                "beta padding padding padding padding padding",
                "user",
            )
        })
        .collect();
    let after = view
        .effective(
            &scope(),
            LexicalDelta {
                pending: &pending,
                removed: &removed,
                complete: true,
            },
        )
        .unwrap()
        .search("alpha beta", 10)
        .unwrap();
    assert_eq!(after[0].memory_id, "a");
    let mut rebuilt = SearchIndex::open(f.db("rebuilt.db")).unwrap();
    let mut current = docs[..2].to_vec();
    current.extend(pending);
    rebuilt.commit(1, &current, &[]).unwrap();
    assert_eq!(
        after,
        rebuilt
            .read_view()
            .unwrap()
            .search("alpha beta", &scope(), 10, &[], &[], true)
            .unwrap()
    );
}

#[test]
fn read_only_view_does_not_write_files() {
    let f = Fixture::new();
    let path = f.db("index.db");
    let mut index = SearchIndex::open(&path).unwrap();
    index
        .commit(1, &[doc("a", "1", "alpha", "user")], &[])
        .unwrap();
    let files = || {
        let mut v: Vec<_> = fs::read_dir(&f.0)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| !p.to_string_lossy().ends_with("-shm"))
            .map(|p| (p.file_name().unwrap().to_owned(), fs::read(&p).unwrap()))
            .collect();
        v.sort();
        v
    };
    let before = files();
    let mut view = ReadView::open(&path).unwrap();
    assert!(view
        .connection
        .is_readonly(rusqlite::DatabaseName::Main)
        .unwrap());
    let temp_store: i64 = view
        .connection
        .query_row("PRAGMA temp_store", [], |row| row.get(0))
        .unwrap();
    assert_eq!(temp_store, 2, "TEMP storage must remain in memory");
    let pending = [doc("b", "1", "beta alpha", "user")];
    let mut corpus = view
        .effective(
            &scope(),
            LexicalDelta {
                pending: &pending,
                removed: &[],
                complete: true,
            },
        )
        .unwrap();
    corpus.search("alpha", 10).unwrap();
    corpus.strength("alpha", 10).unwrap();
    corpus.matched("alpha").unwrap();
    assert_eq!(before, files());
}

#[test]
fn open_pins_stamp_and_content() {
    let f = Fixture::new();
    let path = f.db("index.db");
    let mut index = SearchIndex::open(&path).unwrap();
    index
        .commit(1, &[doc("a", "1", "alpha", "user")], &[])
        .unwrap();
    let mut view = ReadView::open(&path).unwrap();
    let stamp = view.stamp.clone();
    index
        .commit(2, &[doc("b", "1", "alpha", "user")], &[])
        .unwrap();
    assert_eq!(
        view.effective(&scope(), empty())
            .unwrap()
            .matched("alpha")
            .unwrap(),
        1
    );
    assert_eq!(view.stamp, stamp);
    let mut fresh = ReadView::open(&path).unwrap();
    assert_ne!(fresh.stamp, stamp);
    assert_eq!(
        fresh
            .effective(&scope(), empty())
            .unwrap()
            .matched("alpha")
            .unwrap(),
        2
    );
}

#[test]
fn incomplete_delta_is_rejected() {
    let f = Fixture::new();
    let path = f.db("index.db");
    let _index = SearchIndex::open(&path).unwrap();
    let mut view = ReadView::open(&path).unwrap();
    assert!(matches!(
        view.effective(
            &scope(),
            LexicalDelta {
                pending: &[],
                removed: &[],
                complete: false
            }
        ),
        Err(Error::CoverageUnknown)
    ));
}

#[test]
fn repeated_operations_reuse_scratch_and_new_delta_is_isolated() {
    let f = Fixture::new();
    let path = f.db("index.db");
    let mut index = SearchIndex::open(&path).unwrap();
    let base = [
        doc("a", "1", "alpha", "user"),
        doc("b", "1", "beta", "user"),
    ];
    index.commit(1, &base, &[]).unwrap();
    let first = [
        doc("a", "2", "gamma alpha", "user"),
        doc("c", "1", "gamma beta", "user"),
    ];
    let mut expected = SearchIndex::open(f.db("reference.db")).unwrap();
    expected
        .commit(
            1,
            &[base[1].clone(), first[0].clone(), first[1].clone()],
            &[],
        )
        .unwrap();
    let mut reference = expected.read_view().unwrap();
    let mut view = ReadView::open(&path).unwrap();
    {
        let mut corpus = view
            .effective(
                &scope(),
                LexicalDelta {
                    pending: &first,
                    removed: &["a".into()],
                    complete: true,
                },
            )
            .unwrap();
        // Any attempt to repopulate the request's overlay after construction fails.
        corpus.view.connection.execute_batch("CREATE TEMP TRIGGER no_overlay_reinsert BEFORE INSERT ON overlay BEGIN SELECT RAISE(ABORT, 'overlay rebuilt'); END;").unwrap();
        for query in ["gamma", "alpha beta", "missing", "gamma"] {
            assert_eq!(
                corpus.matched(query).unwrap(),
                reference.lexical_matched(query, &scope()).unwrap()
            );
            assert_eq!(
                corpus.strength(query, 10).unwrap(),
                reference.lexical_strength(query, &scope(), 10).unwrap()
            );
            assert_eq!(
                corpus.search(query, 10).unwrap(),
                reference
                    .search(query, &scope(), 10, &[], &[], true)
                    .unwrap()
            );
            assert_eq!(
                corpus.matched(query).unwrap(),
                reference.lexical_matched(query, &scope()).unwrap()
            );
            let trigger: i64 = corpus.view.connection.query_row("SELECT COUNT(*) FROM sqlite_temp_master WHERE type='trigger' AND name='no_overlay_reinsert'", [], |r| r.get(0)).unwrap();
            assert_eq!(trigger, 1, "overlay was dropped and rebuilt");
        }
    }
    let scratch: i64 = view.connection.query_row("SELECT count(*) FROM sqlite_temp_master WHERE name IN ('overlay','op','invalid','scopes','query','idfs','term_lookup','effective','effective_postings')", [], |r| r.get(0)).unwrap();
    assert_eq!(scratch, 0);
    let second = [doc("b", "2", "delta", "user")];
    let mut rebuilt = SearchIndex::open(f.db("reference2.db")).unwrap();
    rebuilt
        .commit(1, &[base[0].clone(), second[0].clone()], &[])
        .unwrap();
    let mut reference2 = rebuilt.read_view().unwrap();
    let mut corpus = view
        .effective(
            &scope(),
            LexicalDelta {
                pending: &second,
                removed: &["b".into()],
                complete: true,
            },
        )
        .unwrap();
    for query in ["gamma", "alpha delta"] {
        assert_eq!(
            corpus.search(query, 10).unwrap(),
            reference2
                .search(query, &scope(), 10, &[], &[], true)
                .unwrap()
        );
        assert_eq!(
            corpus.matched(query).unwrap(),
            reference2.lexical_matched(query, &scope()).unwrap()
        );
        assert_eq!(
            corpus.strength(query, 10).unwrap(),
            reference2.lexical_strength(query, &scope(), 10).unwrap()
        );
    }
}
