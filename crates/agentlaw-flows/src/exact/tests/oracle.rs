use super::*;
use agentlaw_storage::{Head, Mutation, Store};
use serde_json::json;

const MACHINE: &str = "33333333-3333-4333-8333-333333333333";
const PROJECT: &str = "44444444-4444-4444-8444-444444444444";

fn unit(body: &str) -> CurrentUnit {
    CurrentUnit {
        entity_id: uuid::Uuid::new_v4().to_string(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":MACHINE},"recorded_at_ms":0,"is_rule":false,"in_working_set":false,"relations":[],"work_targets":[]}),
                body: body.into(),
            }],
        },
    }
}
fn head(unit: &mut CurrentUnit) -> &mut Head {
    match &mut unit.state {
        UnitState::Live { heads } => &mut heads[0],
        _ => panic!(),
    }
}
fn publish(store: &Store, unit: CurrentUnit) {
    let old = if unit.entity_type == "memory" {
        store.read_current(&unit.entity_id)
    } else {
        store.read_procedure(&unit.entity_id)
    };
    let versions = old
        .map(|u| {
            u.references()
                .unwrap()
                .into_iter()
                .map(|r| r.observed_version)
                .collect()
        })
        .unwrap_or_default();
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit,
                expected_versions: versions,
                evidence: "isolated reviewer fixture".into(),
            }],
        )
        .unwrap();
}
fn fixture(dir: &Path) -> (Store, ProjectionGenerations, ExactIndexWriter) {
    let store = Store::open(dir.join("source"), dir.join("canonical")).unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let generation = generation(dir, "audit", "lexical-audit.sqlite");
    let connection = Connection::open(&generation.exact_file).unwrap();
    connection
        .execute(
            "UPDATE exact_position SET epoch=?1,sequence=?2",
            params![position.epoch, position.sequence.to_string()],
        )
        .unwrap();
    drop(connection);
    let writer = ExactIndexWriter::open(&generation.exact_file).unwrap();
    let registry = ProjectionGenerations::open(
        "audit",
        &position.epoch,
        &generation.exact_file,
        &generation.lexical_file,
    )
    .unwrap();
    (store, registry, writer)
}
fn context() -> RequestContext {
    RequestContext {
        store_binding_id: "review".into(),
        user_id: "user".into(),
        machine_id: MACHINE.into(),
        project_id: Some(PROJECT.into()),
        connection_version: "1".into(),
    }
}
fn snapshot(
    registry: &ProjectionGenerations,
    source: &agentlaw_storage::published::OwnedPublishedReader,
) -> ExactSnapshot {
    let pinned = registry.pin().unwrap();
    let guard = source.read_snapshot().unwrap();
    ExactSnapshot::build(pinned, &guard, DeltaBudget::default()).unwrap()
}
fn canonical_heads(mut heads: Vec<HeadRecord>) -> Vec<(String, String, String, String, String)> {
    heads.sort_by(|a, b| {
        (&a.entity_id, &a.observed_version).cmp(&(&b.entity_id, &b.observed_version))
    });
    heads
        .into_iter()
        .map(|h| {
            (
                h.entity_id,
                h.entity_type,
                h.observed_version,
                h.metadata.to_string(),
                h.excerpt,
            )
        })
        .collect()
}

#[test]
fn review_mixed_delta_matches_rebuilt_exact_and_lexical() {
    let dir = tempfile::tempdir().unwrap();
    let (store, registry, mut writer) = fixture(dir.path());
    let source = store.owned_published_reader();
    let mut anchor = unit("anchor quartz common");
    head(&mut anchor).metadata["is_rule"] = json!(true);
    publish(&store, anchor.clone());
    let mut changing = unit("oldtopaz common common");
    head(&mut changing).metadata["is_rule"] = json!(true);
    head(&mut changing).metadata["relations"] =
        json!([{"kind":"required","target_memory_id":anchor.entity_id}]);
    head(&mut changing).metadata["work_targets"] =
        json!([{"project_id":PROJECT,"path":"src/old.rs","kind":"file","reading":"required"}]);
    publish(&store, changing.clone());
    let mut deleted = unit("obsolete deleted coral");
    head(&mut deleted).metadata["is_rule"] = json!(true);
    publish(&store, deleted.clone());
    let mut procedure = unit("procedure instructions unchanged digest");
    procedure.entity_type = "learned_procedure".into();
    head(&mut procedure).metadata["name"] = json!("oldprocedure");
    head(&mut procedure).metadata["use_when"] = json!("old usage");
    publish(&store, procedure.clone());
    writer
        .synchronize(&source, &RequestControl::default())
        .unwrap();
    let pinned = registry.pin().unwrap();
    head(&mut changing).metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    head(&mut changing).metadata["is_rule"] = json!(false);
    head(&mut changing).metadata["in_working_set"] = json!(true);
    head(&mut changing).metadata["relations"] =
        json!([{"kind":"related","target_memory_id":anchor.entity_id}]);
    head(&mut changing).metadata["work_targets"] =
        json!([{"project_id":PROJECT,"path":"src/new.rs","kind":"file","reading":"related"}]);
    head(&mut changing).metadata["applicability"] = json!({"scope":"machine","machine_id":MACHINE});
    head(&mut changing).body = "freshamber common".into();
    publish(&store, changing.clone());
    let mut added = unit("newemerald common");
    head(&mut added).metadata["is_rule"] = json!(true);
    head(&mut added).metadata["relations"] =
        json!([{"kind":"required","target_memory_id":anchor.entity_id}]);
    publish(&store, added.clone());
    // Multihead source fixture follows the test-plan's direct-current construction.
    let mut second = added.heads()[0].clone();
    second.metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    second.metadata["applicability"] = json!({"scope":"machine","machine_id":MACHINE});
    second.metadata["is_rule"] = json!(false);
    second.body = "multisapphire common common common".into();
    if let UnitState::Live { heads } = &mut added.state {
        heads.push(second);
    }
    std::fs::write(
        dir.path().join(format!(
            "source/current/memory/{}/{}.md",
            &added.entity_id[..2],
            added.entity_id
        )),
        added.encode().unwrap(),
    )
    .unwrap();
    // A ledger-covered removal fixture, confined to this throwaway source.
    head(&mut deleted).metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    head(&mut deleted).body = "before removal".into();
    publish(&store, deleted.clone());
    std::fs::remove_file(dir.path().join(format!(
        "source/current/memory/{}/{}.md",
        &deleted.entity_id[..2],
        deleted.entity_id
    )))
    .unwrap();
    head(&mut procedure).metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    head(&mut procedure).metadata["name"] = json!("newprocedure common");
    head(&mut procedure).metadata["use_when"] = json!("new usage");
    head(&mut procedure).metadata["applicability"] =
        json!({"scope":"machine","machine_id":MACHINE});
    publish(&store, procedure.clone());
    let guard = source.read_snapshot().unwrap();
    let mut stale = ExactSnapshot::build(pinned, &guard, DeltaBudget::default()).unwrap();
    drop(guard);
    let shadow = writer
        .rebuild_shadow(&source, &RequestControl::default())
        .unwrap();
    let fresh_registry = ProjectionGenerations::from_generation(shadow.generation).unwrap();
    let mut fresh = snapshot(&fresh_registry, &source);
    let ids = vec![
        anchor.entity_id.clone(),
        changing.entity_id.clone(),
        added.entity_id.clone(),
        deleted.entity_id.clone(),
        procedure.entity_id.clone(),
    ];
    for ctx in [
        context(),
        RequestContext {
            machine_id: uuid::Uuid::new_v4().to_string(),
            ..context()
        },
    ] {
        for (rules, tasks) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(
                stale.ids_matching(&ctx, rules, tasks).unwrap(),
                fresh.ids_matching(&ctx, rules, tasks).unwrap()
            );
        }
        assert_eq!(
            stale.procedure_ids(&ctx).unwrap(),
            fresh.procedure_ids(&ctx).unwrap()
        );
    }
    assert_eq!(stale.related(&ids).unwrap(), fresh.related(&ids).unwrap());
    assert_eq!(
        stale.reverse_required(&ids).unwrap(),
        fresh.reverse_required(&ids).unwrap()
    );
    assert_eq!(
        canonical_heads(stale.heads(&ids).unwrap()),
        canonical_heads(fresh.heads(&ids).unwrap())
    );
    for path in ["src", "src/old.rs", "src/new.rs", "src/no-match"] {
        let target = [WorkTarget {
            path: path.into(),
            kind: TargetKind::Directory,
        }];
        assert_eq!(
            stale.work_targets(PROJECT, &target).unwrap(),
            fresh.work_targets(PROJECT, &target).unwrap()
        );
    }
    for body in [
        "oldtopaz common common",
        "freshamber common",
        "newemerald common",
        "multisapphire common common common",
        "obsolete deleted coral",
        "procedure instructions unchanged digest",
    ] {
        assert_eq!(
            stale.identical_bodies(body).unwrap(),
            fresh.identical_bodies(body).unwrap()
        );
    }
    for u in [&changing, &added, &procedure] {
        for (h, r) in u.heads().iter().zip(u.references().unwrap()) {
            assert_eq!(
                stale.owned_body(&u.entity_id, &r.observed_version),
                Some(h.body.as_str())
            );
        }
    }
    for scopes in [
        vec!["user".into()],
        vec![format!("machine:{MACHINE}")],
        vec!["user".into(), format!("machine:{MACHINE}")],
        vec!["procedure:user".into()],
    ] {
        let scope = agentlaw_search::ScopeFilter {
            allowed_scopes: scopes,
        };
        for query in [
            "oldtopaz",
            "common",
            "freshamber quartz",
            "multisapphire",
            "newprocedure",
            "oldprocedure",
            "obsolete",
        ] {
            let mut a = stale.lexical(&scope).unwrap();
            let mut b = fresh.lexical(&scope).unwrap();
            assert_eq!(
                a.search(query, 100).unwrap(),
                b.search(query, 100).unwrap(),
                "search {query}"
            );
            assert_eq!(
                a.strength(query, 100).unwrap(),
                b.strength(query, 100).unwrap(),
                "strength {query}"
            );
            assert_eq!(
                a.matched(query).unwrap(),
                b.matched(query).unwrap(),
                "matched {query}"
            );
        }
    }
}

type PinHook = Box<dyn FnOnce() + Send>;
static PIN_HOOK: Mutex<Option<(uuid::Uuid, PinHook)>> = Mutex::new(None);
pub(in crate::exact) fn on_pointer_mismatch(registry: &ProjectionGenerations) {
    let hook = {
        let mut slot = PIN_HOOK.lock().unwrap();
        if slot.as_ref().is_some_and(|(id, _)| *id == registry.id) {
            slot.take()
        } else {
            None
        }
    };
    if let Some((_, hook)) = hook {
        hook();
    }
}

#[test]
fn pointer_mismatch_retries_after_concurrent_generation_commit() {
    let dir = tempfile::tempdir().unwrap();
    let old = generation(dir.path(), "mismatch", "lexical-wrong.sqlite");
    let new = generation(dir.path(), "replacement", "lexical-replacement.sqlite");
    let registry = Arc::new(ProjectionGenerations::from_generation(old).unwrap());
    let (mismatch_tx, mismatch_rx) = std::sync::mpsc::channel();
    let (committed_tx, committed_rx) = std::sync::mpsc::channel();
    *PIN_HOOK.lock().unwrap() = Some((
        registry.id,
        Box::new(move || {
            mismatch_tx.send(()).unwrap();
            committed_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }),
    ));
    let updater = registry.clone();
    let thread = std::thread::spawn(move || {
        mismatch_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        updater
            .commit(updater.stage(ShadowGeneration { generation: new }).unwrap())
            .unwrap();
        assert!(updater.take_retired().is_empty());
        committed_tx.send(()).unwrap();
    });
    let pinned = registry.pin();
    thread.join().unwrap();
    assert_eq!(pinned.unwrap().generation.id, "replacement");
    assert_eq!(registry.take_retired().len(), 1);
}

#[test]
fn writer_bounded_batches_resume_after_interruption_and_match_rebuild() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let (store, _registry, mut writer) = fixture(dir.path());
    let source = store.owned_published_reader();
    let first = unit("small body 0");
    let mut ids = vec![first.entity_id.clone()];
    publish(&store, first);
    let journal = Connection::open(dir.path().join("canonical/journal.sqlite")).unwrap();
    let raw: String = journal
        .query_row(
            "SELECT manifest FROM publications WHERE sequence=?1",
            [1u64.to_be_bytes().as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    let mut manifest: serde_json::Value = serde_json::from_str(&raw).unwrap();
    for i in 1..2048 {
        let unit = unit(&format!("small body {i}"));
        ids.push(unit.entity_id.clone());
        let relative = format!(
            "current/memory/{}/{}.md",
            &unit.entity_id[..2],
            unit.entity_id
        );
        let path = dir.path().join("source").join(&relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, unit.encode().unwrap()).unwrap();
        let mut target = manifest["targets"][0].clone();
        target["path"] = json!(relative);
        manifest["targets"].as_array_mut().unwrap().push(target);
    }
    journal
        .execute(
            "UPDATE publications SET manifest=?1",
            [manifest.to_string()],
        )
        .unwrap();
    drop(journal);
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    let control = RequestControl::new(cancel, move |phase| {
        if phase == "exact_batch_committed" {
            signal.store(true, Ordering::Release);
        }
    });
    let (interrupted, first_peak) =
        super::memory::measure(65_536, || writer.synchronize(&source, &control));
    assert_eq!(interrupted.unwrap_err().code, "cancelled");
    assert_eq!(writer.exact.position().unwrap().unwrap().sequence, 0);
    let (result, resumed_peak) = super::memory::measure(65_536, || {
        writer.synchronize(&source, &RequestControl::default())
    });
    eprintln!("writer large-allocation peaks: first={first_peak} resumed={resumed_peak} bytes");
    assert_eq!(result.unwrap().sequence, 1);
    assert!(
        first_peak.max(resumed_peak) < 32 * 1024 * 1024,
        "writer allocation peak {}",
        first_peak.max(resumed_peak)
    );
    let shadow = writer
        .rebuild_shadow(&source, &RequestControl::default())
        .unwrap();
    let fresh = ProjectionGenerations::from_generation(shadow.generation).unwrap();
    let mut rebuilt = snapshot(&fresh, &source);
    assert_eq!(
        canonical_heads(writer.exact.heads(&ids).unwrap()),
        canonical_heads(rebuilt.heads(&ids).unwrap())
    );
    let scope = agentlaw_search::ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    let mut active = writer.lexical.read_view().unwrap();
    let left = active
        .effective(
            &scope,
            agentlaw_search::LexicalDelta {
                pending: &[],
                removed: &[],
                complete: true,
            },
        )
        .unwrap()
        .matched("small")
        .unwrap();
    let right = rebuilt.lexical(&scope).unwrap().matched("small").unwrap();
    assert_eq!((left, right), (2048, 2048));
}
#[test]
fn review_synchronize_recovers_between_lexical_and_exact_commits() {
    let dir = tempfile::tempdir().unwrap();
    let (store, registry, mut writer) = fixture(dir.path());
    let source = store.owned_published_reader();
    let mut u = unit("interrupted unique rule");
    head(&mut u).metadata["is_rule"] = json!(true);
    publish(&store, u.clone());
    writer.exact.connection.execute_batch("CREATE TRIGGER reviewer_fail_position BEFORE UPDATE ON exact_position BEGIN SELECT RAISE(ABORT,'review injected exact failure'); END;").unwrap();
    let err = writer
        .synchronize(&source, &RequestControl::default())
        .unwrap_err();
    assert_eq!(err.code, "exact_index_failed");
    let al = writer.lexical.read_view().unwrap().stamp.ack as u64;
    let ae = writer.exact.position().unwrap().unwrap().sequence;
    assert!(al > ae);
    let mut during = snapshot(&registry, &source);
    assert_eq!(
        during.ids_matching(&context(), true, false).unwrap(),
        vec![u.entity_id.clone()]
    );
    assert_eq!(
        during
            .lexical(&agentlaw_search::ScopeFilter {
                allowed_scopes: vec!["user".into()]
            })
            .unwrap()
            .matched("interrupted")
            .unwrap(),
        1
    );
    let path = dir.path().join("exact-audit.sqlite");
    drop(writer);
    let c = Connection::open(&path).unwrap();
    c.execute_batch("DROP TRIGGER reviewer_fail_position")
        .unwrap();
    drop(c);
    let mut reopened = ExactIndexWriter::open(&path).unwrap();
    let through = reopened
        .synchronize(&source, &RequestControl::default())
        .unwrap();
    assert_eq!(through.sequence, al);
    assert_eq!(
        reopened
            .exact
            .ids_matching(&context(), true, false)
            .unwrap(),
        vec![u.entity_id]
    );
    assert!(!dir.path().join("exact-maintenance.sqlite").exists());
}

#[test]
fn procedure_budget_counts_owned_body_not_lexical_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let (store, registry, _) = fixture(dir.path());
    let source = store.owned_published_reader();
    let mut procedure = unit("small instructions");
    procedure.entity_type = "learned_procedure".into();
    head(&mut procedure).metadata["name"] = json!("a much longer searchable procedure title");
    head(&mut procedure).metadata["use_when"] = json!("an additional searchable description");
    publish(&store, procedure);
    let guard = source.read_snapshot().unwrap();
    let budget = DeltaBudget {
        max_identities: 1,
        max_resident_bytes: "small instructions".len() as u64,
    };
    assert!(ExactSnapshot::build(registry.pin().unwrap(), &guard, budget).is_ok());
}

#[test]
fn writer_opens_each_head_stream_only_when_consumed() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, _) = fixture(dir.path());
    let source = store.owned_published_reader();
    let mut current = unit("first head");
    publish(&store, current.clone());
    let mut second = current.heads()[0].clone();
    second.metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    second.body = "second head".into();
    let second_id = second.metadata["change_id"].as_str().unwrap().to_owned();
    if let UnitState::Live { heads } = &mut current.state {
        heads.push(second);
    }
    let path = dir.path().join(format!(
        "source/current/memory/{}/{}.md",
        &current.entity_id[..2],
        current.entity_id
    ));
    std::fs::write(path, current.encode().unwrap()).unwrap();
    let guard = source.read_snapshot().unwrap();
    let acquired = guard.acquire_current(&current.entity_id).unwrap();
    drop(guard);
    let indexed = acquired_unit(&acquired).unwrap();
    std::fs::remove_file(&acquired.bodies[&second_id].path).unwrap();
    let mut stream = lazy_head_streams(
        std::slice::from_ref(&indexed),
        std::slice::from_ref(&acquired),
    );
    assert_eq!(stream.next().unwrap().unwrap().body, "first head");
    assert!(stream.any(|item| item.is_err()));
}

#[test]
fn oversized_identity_is_single_batch_and_matches_rebuild() {
    use std::io::{BufWriter, Cursor, Write};
    use std::sync::atomic::AtomicBool;
    let dir = tempfile::tempdir().unwrap();
    let (store, _, mut writer) = fixture(dir.path());
    let source = store.owned_published_reader();
    let mut near = unit("near seed");
    near.entity_id = "11111111-1111-4111-8111-111111111111".into();
    let mut large = unit("large seed");
    large.entity_id = "22222222-2222-4222-8222-222222222222".into();
    let mut empty = unit("");
    empty.entity_id = "33333333-3333-4333-8333-333333333333".into();
    publish(&store, near.clone());
    for (current, bytes, token) in [
        (&near, 63 * 1024 * 1024, b"nearword ".as_slice()),
        (&large, 65 * 1024 * 1024, b"bigterm ".as_slice()),
    ] {
        let path = dir.path().join(format!(
            "source/current/memory/{}/{}.md",
            &current.entity_id[..2],
            current.entity_id
        ));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut frames =
            agentlaw_storage::codec::read("current", &mut Cursor::new(current.encode().unwrap()))
                .unwrap();
        frames[1].payload = vec![b'x'; bytes];
        frames[1].payload[..token.len()].copy_from_slice(token);
        let mut output = BufWriter::new(std::fs::File::create(path).unwrap());
        agentlaw_storage::codec::write("current", &frames, &mut output).unwrap();
        output.flush().unwrap();
    }
    let empty_path = dir.path().join(format!(
        "source/current/memory/{}/{}.md",
        &empty.entity_id[..2],
        empty.entity_id
    ));
    std::fs::create_dir_all(empty_path.parent().unwrap()).unwrap();
    std::fs::write(empty_path, empty.encode().unwrap()).unwrap();
    let journal = Connection::open(dir.path().join("canonical/journal.sqlite")).unwrap();
    let raw: String = journal
        .query_row("SELECT manifest FROM publications", [], |row| row.get(0))
        .unwrap();
    let mut manifest: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let mut target = manifest["targets"][0].clone();
    target["path"] = json!(format!(
        "current/memory/{}/{}.md",
        &large.entity_id[..2],
        large.entity_id
    ));
    manifest["targets"].as_array_mut().unwrap().push(target);
    let mut target = manifest["targets"][0].clone();
    target["path"] = json!(format!(
        "current/memory/{}/{}.md",
        &empty.entity_id[..2],
        empty.entity_id
    ));
    manifest["targets"].as_array_mut().unwrap().push(target);
    journal
        .execute(
            "UPDATE publications SET manifest=?1",
            [manifest.to_string()],
        )
        .unwrap();
    drop(journal);
    let progress = Arc::new(Mutex::new(Vec::new()));
    let observed = progress.clone();
    let exact_path = dir.path().join("exact-audit.sqlite");
    let control = RequestControl::new(Arc::new(AtomicBool::new(false)), move |phase| {
        if phase == "exact_batch_committed" {
            let db = Connection::open_with_flags(
                &exact_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let ack: String = db
                .query_row("SELECT sequence FROM exact_position", [], |r| r.get(0))
                .unwrap();
            let heads: u64 = db
                .query_row("SELECT count(*) FROM exact_heads", [], |r| r.get(0))
                .unwrap();
            observed.lock().unwrap().push((ack, heads));
        }
    });
    let (result, peak) =
        super::memory::measure(1024 * 1024, || writer.synchronize(&source, &control));
    assert_eq!(result.unwrap().sequence, 1);
    assert_eq!(
        *progress.lock().unwrap(),
        vec![("0".into(), 1), ("0".into(), 2), ("1".into(), 3)]
    );
    assert!(peak <= 69 * 1024 * 1024, "large-allocation peak {peak}");
    let ids = [near.entity_id, large.entity_id, empty.entity_id];
    let expected = canonical_heads(writer.exact.heads(&ids).unwrap());
    let shadow = writer
        .rebuild_shadow(&source, &RequestControl::default())
        .unwrap();
    let registry = ProjectionGenerations::from_generation(shadow.generation).unwrap();
    let mut rebuilt = snapshot(&registry, &source);
    assert_eq!(canonical_heads(rebuilt.heads(&ids).unwrap()), expected);
    let scope = agentlaw_search::ScopeFilter {
        allowed_scopes: vec!["user".into()],
    };
    assert_eq!(
        rebuilt.lexical(&scope).unwrap().matched("bigterm").unwrap(),
        1
    );
    let mut view = writer.lexical.read_view().unwrap();
    assert_eq!(
        view.effective(
            &scope,
            agentlaw_search::LexicalDelta {
                pending: &[],
                removed: &[],
                complete: true
            }
        )
        .unwrap()
        .matched("bigterm")
        .unwrap(),
        1
    );
}
