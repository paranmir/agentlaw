use agentlaw_storage::*;
use serde_json::json;
use std::io::Cursor;
const ID: &str = "11111111-1111-4111-8111-111111111111";

#[test]
fn competing_writers_cannot_both_publish_from_the_same_observed_version() {
    use std::sync::{Arc, Barrier};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let store = Store::open(&root, &local).unwrap();
    let original = store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(unit(ID, "original"))],
        )
        .unwrap();
    let version = original.references[0].observed_version.clone();
    let barrier = Arc::new(Barrier::new(2));
    let outcomes = std::thread::scope(|scope| {
        let handles: Vec<_> = ["writer A", "writer B"]
            .into_iter()
            .map(|body| {
                let s = Store::open(&root, &local).unwrap();
                let barrier = barrier.clone();
                let version = version.clone();
                scope.spawn(move || {
                    let mut change = unit(ID, body);
                    if let UnitState::Live { heads } = &mut change.state {
                        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
                    }
                    barrier.wait();
                    (
                        body,
                        s.publish(
                            &uuid::Uuid::new_v4().to_string(),
                            vec![Mutation {
                                unit: change,
                                expected_versions: vec![version],
                                evidence: "Concurrent update".into(),
                            }],
                        ),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    let winners: Vec<_> = outcomes.iter().filter(|(_, r)| r.is_ok()).collect();
    assert_eq!(
        winners.len(),
        1,
        "exactly one compare-and-publish must succeed: {outcomes:?}"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|(_, r)| matches!(r, Err(Error::Stale(_))))
            .count(),
        1
    );
    assert_eq!(
        store.read_current(ID).unwrap().heads()[0].body,
        winners[0].0
    );
    assert_eq!(
        store.history(ID).unwrap().len(),
        2,
        "losing write must not create hidden history"
    );
}

#[test]
fn interrupted_update_batch_recovers_all_members_and_keeps_each_previous_version() {
    let other = "44444444-4444-4444-8444-444444444444";
    for point in [
        FaultPoint::Prepared,
        FaultPoint::DirtyFence,
        FaultPoint::Decision,
        FaultPoint::Installed(0),
        FaultPoint::Published,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        let local = temp.path().join("local");
        let s = Store::open(&root, &local).unwrap();
        let mut initial_b = unit(other, "B before");
        if let UnitState::Live { heads } = &mut initial_b.state {
            heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
        }
        s.publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(unit(ID, "A before")), mutation(initial_b)],
        )
        .unwrap();
        let changes = [(ID, "A after"), (other, "B after")]
            .into_iter()
            .map(|(id, body)| {
                let current = s.read_current(id).unwrap();
                let mut change = unit(id, body);
                if let UnitState::Live { heads } = &mut change.state {
                    heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
                }
                Mutation {
                    unit: change,
                    expected_versions: current
                        .references()
                        .unwrap()
                        .into_iter()
                        .map(|r| r.observed_version)
                        .collect(),
                    evidence: "Related update batch".into(),
                }
            })
            .collect();
        assert!(s
            .publish_with_fault(&uuid::Uuid::new_v4().to_string(), changes, Some(point))
            .is_err());
        drop(s);
        let s = Store::open(&root, &local).unwrap();
        let before = matches!(point, FaultPoint::Prepared | FaultPoint::DirtyFence);
        assert_eq!(
            s.read_current(ID).unwrap().heads()[0].body,
            if before { "A before" } else { "A after" },
            "{point:?}"
        );
        assert_eq!(
            s.read_current(other).unwrap().heads()[0].body,
            if before { "B before" } else { "B after" },
            "{point:?}"
        );
        for (id, original) in [(ID, "A before"), (other, "B before")] {
            let history = s.history(id).unwrap();
            assert_eq!(history.len(), if before { 1 } else { 2 }, "{point:?}");
            assert!(history.iter().any(|change| change.body == original));
        }
    }
}
#[test]
fn corrupt_journal_repair_keeps_original_and_finishes_durable_decision() {
    for interrupted in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        let local = temp.path().join("local");
        let s = Store::open(&root, &local).unwrap();
        let op = uuid::Uuid::new_v4().to_string();
        let result = s.publish_with_fault(
            &op,
            vec![mutation(unit(ID, "never lost"))],
            interrupted.then_some(FaultPoint::Decision),
        );
        if interrupted {
            assert!(matches!(
                result,
                Err(Error::Interrupted(FaultPoint::Decision))
            ));
        } else {
            result.unwrap();
        }
        drop(s);
        std::fs::write(
            local.join("pending-sentinel"),
            b"pending authority untouched",
        )
        .unwrap();
        std::fs::write(
            local.join("journal.sqlite"),
            b"damaged original sqlite bytes",
        )
        .unwrap();
        let report = Store::repair_local_journal(&root, &local).unwrap();
        assert_eq!(report.source_generation, 1);
        assert_eq!(
            std::fs::read(report.backup_directory.join("original-journal.sqlite")).unwrap(),
            b"damaged original sqlite bytes"
        );
        assert_eq!(
            std::fs::read(local.join("pending-sentinel")).unwrap(),
            b"pending authority untouched"
        );
        let s = Store::open(&root, &local).unwrap();
        assert_eq!(s.read_current(ID).unwrap().heads()[0].body, "never lost");
        assert!(local.join("recovery").join(op).join("published").exists());
    }
}
#[test]
fn corrupt_journal_missing_authority_does_not_replace_original() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let local = temp.path().join("local");
    let s = Store::open(&root, &local).unwrap();
    let op = uuid::Uuid::new_v4().to_string();
    s.publish(&op, vec![mutation(unit(ID, "intact source"))])
        .unwrap();
    drop(s);
    std::fs::rename(
        local.join("recovery").join(&op).join("published"),
        local.join("recovery").join(&op).join("held-published"),
    )
    .unwrap();
    std::fs::write(local.join("journal.sqlite"), b"original damaged bytes").unwrap();
    assert!(matches!(
        Store::repair_local_journal(&root, &local),
        Err(Error::RecoveryRequired(_))
    ));
    assert_eq!(
        std::fs::read(local.join("journal.sqlite")).unwrap(),
        b"original damaged bytes"
    );
}
#[test]
fn import_union_keeps_concurrent_heads_until_explicit_resolution() {
    fn copy(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to.join(e.file_name()));
            } else {
                std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }
    fn updated(body: &str) -> CurrentUnit {
        let mut u = unit(ID, body);
        if let UnitState::Live { heads } = &mut u.state {
            heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
        }
        u
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("active");
    let active = Store::open(&root, temp.path().join("active-local")).unwrap();
    let base = unit(ID, "common");
    let refs = base
        .references()
        .unwrap()
        .into_iter()
        .map(|r| r.observed_version)
        .collect::<Vec<_>>();
    active
        .publish(&uuid::Uuid::new_v4().to_string(), vec![mutation(base)])
        .unwrap();
    let incoming_root = temp.path().join("incoming");
    copy(&root, &incoming_root);
    let incoming =
        Store::attach_existing(&incoming_root, temp.path().join("incoming-local")).unwrap();
    for (s, body) in [(&active, "left"), (&incoming, "right")] {
        s.publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit: updated(body),
                expected_versions: refs.clone(),
                evidence: "independent explicit change".into(),
            }],
        )
        .unwrap();
    }
    let staged = active
        .prepare_import_union(
            &incoming,
            temp.path().join("review"),
            temp.path().join("review-local"),
        )
        .unwrap();
    let heads = staged.read_current(ID).unwrap();
    assert_eq!(heads.heads().len(), 2);
    assert_eq!(active.read_current(ID).unwrap().heads()[0].body, "left");
    staged
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit: updated("user approved merged meaning"),
                expected_versions: heads
                    .references()
                    .unwrap()
                    .into_iter()
                    .map(|r| r.observed_version)
                    .collect(),
                evidence: "user approved resolution".into(),
            }],
        )
        .unwrap();
    active
        .publish_imported_source(&uuid::Uuid::new_v4().to_string(), &staged, 2)
        .unwrap();
    assert_eq!(
        active.read_current(ID).unwrap().heads()[0].body,
        "user approved merged meaning"
    );
    assert_eq!(active.history(ID).unwrap().len(), 4);
}
#[test]
fn resource_admission_preserves_source_and_never_blocks_decided_redo() {
    use agentlaw_storage::resource::ResourceLimits;
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let op = uuid::Uuid::new_v4().to_string();
    let change = unit(ID, "capacity test");
    for limits in [
        ResourceLimits {
            memory_bytes: Some(0),
            disk_bytes: None,
        },
        ResourceLimits {
            memory_bytes: None,
            disk_bytes: Some(0),
        },
    ] {
        assert!(matches!(
            s.publish_with_resource_limits(&op, vec![mutation(change.clone())], &limits),
            Err(Error::InsufficientResource { .. })
        ));
        assert_eq!(s.generation().unwrap(), 0);
        assert!(matches!(s.read_current(ID), Err(Error::NotFound(_))));
    }
    assert!(matches!(
        s.publish_with_fault(
            &op,
            vec![mutation(change.clone())],
            Some(FaultPoint::Decision)
        ),
        Err(Error::Interrupted(FaultPoint::Decision))
    ));
    let receipt = s
        .publish_with_resource_limits(
            &op,
            vec![mutation(change)],
            &ResourceLimits {
                memory_bytes: Some(0),
                disk_bytes: Some(0),
            },
        )
        .unwrap();
    assert_eq!(receipt.generation, 1);
    assert_eq!(s.read_current(ID).unwrap().heads()[0].body, "capacity test");
}
#[test]
fn cache_gc_respects_live_acquisition_lease() {
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    s.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![mutation(unit(ID, "held cache"))],
    )
    .unwrap();
    drop(s.spool_history().unwrap());
    let lease = s.spool_history().unwrap();
    let path = lease.path().to_owned();
    for _ in 0..3 {
        let mut u = unit(&uuid::Uuid::new_v4().to_string(), "new");
        if let UnitState::Live { heads } = &mut u.state {
            heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
        }
        s.publish(&uuid::Uuid::new_v4().to_string(), vec![mutation(u)])
            .unwrap();
    }
    s.maintain_owned_material().unwrap();
    assert!(path.exists());
    drop(lease);
    assert_eq!(
        s.maintain_owned_material().unwrap().removed_history_caches,
        1
    );
    assert!(!path.exists());
}
#[test]
fn unchanged_history_acquisition_reuses_validated_disk_cache() {
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    s.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![mutation(unit(ID, "cache"))],
    )
    .unwrap();
    let first = s.spool_history().unwrap();
    assert!(!first.cache_reused);
    drop(first);
    let mut second = s.spool_history().unwrap();
    assert!(second.cache_reused);
    let mut bodies = Vec::new();
    second
        .visit_closure(ID, |c| {
            bodies.push(c.body.read_owned()?);
            Ok(())
        })
        .unwrap();
    assert_eq!(bodies, vec![b"cache".to_vec()]);
}
#[test]
fn acquired_closure_includes_required_and_lease_cleans_history() {
    use std::io::Read;
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let required = uuid::Uuid::new_v4().to_string();
    let mut parent = unit(ID, "parent");
    if let UnitState::Live { heads } = &mut parent.state {
        heads[0].metadata["relations"] = json!([{"kind":"required","target_memory_id":required}]);
    }
    let mut child = unit(&required, "child");
    if let UnitState::Live { heads } = &mut child.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    s.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![mutation(parent), mutation(child)],
    )
    .unwrap();
    let (_, units, missing) = s.acquire_closure(&[ID.into()]).unwrap();
    assert!(missing.is_empty());
    assert_eq!(units.len(), 2);
    for u in units {
        for body in u.current.bodies.values() {
            let mut bytes = Vec::new();
            body.open().unwrap().read_to_end(&mut bytes).unwrap();
            assert!(!bytes.is_empty());
        }
    }
    let mut retained = None;
    s.visit_history_closure(ID, |change| {
        retained = Some(change.body);
        Ok(())
    })
    .unwrap();
    let body = retained.unwrap();
    let path = body.spool.clone();
    assert!(path.exists());
    assert_eq!(body.read_owned().unwrap(), b"parent");
    drop(body);
    assert!(!path.exists());
}
#[test]
fn imported_source_preserves_heads_and_redoes_after_decision() {
    fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                copy_tree(&e.path(), &to.join(e.file_name()));
            } else {
                std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("source");
    let local = t.path().join("local");
    let s = Store::open(&root, &local).unwrap();
    let old = unit(ID, "before import");
    s.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![mutation(old.clone())],
    )
    .unwrap();
    let staged_root = t.path().join("staged");
    copy_tree(&root, &staged_root);
    let staged = Store::attach_existing(&staged_root, t.path().join("staged-control")).unwrap();
    let mut new = unit(ID, "approved edit");
    if let UnitState::Live { heads } = &mut new.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    staged
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit: new,
                expected_versions: old
                    .references()
                    .unwrap()
                    .into_iter()
                    .map(|r| r.observed_version)
                    .collect(),
                evidence: "approved in isolated workspace".into(),
            }],
        )
        .unwrap();
    let op = uuid::Uuid::new_v4().to_string();
    assert!(matches!(
        s.publish_imported_source_with_fault(&op, &staged, 1, Some(FaultPoint::Decision)),
        Err(Error::Interrupted(FaultPoint::Decision))
    ));
    drop(s);
    let s = Store::open(&root, &local).unwrap();
    assert_eq!(s.read_current(ID).unwrap().heads()[0].body, "approved edit");
    assert_eq!(s.history(ID).unwrap().len(), 2);
    assert_eq!(
        s.publish_imported_source(&op, &staged, 1)
            .unwrap()
            .generation,
        2
    );
}
#[test]
fn completed_images_gc_preserves_retry_and_ledger_rebuild() {
    use agentlaw_storage::published::PublishedChangeSource;
    let t = tempfile::tempdir().unwrap();
    let local = t.path().join("local");
    let s = Store::open(t.path().join("source"), &local).unwrap();
    let basis = s.published_reader().position().unwrap();
    let op = uuid::Uuid::new_v4().to_string();
    let original = unit(ID, "first");
    s.publish(&op, vec![mutation(original.clone())]).unwrap();
    let mut second = unit(ID, "second");
    if let UnitState::Live { heads } = &mut second.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    s.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![Mutation {
            unit: second,
            expected_versions: original
                .references()
                .unwrap()
                .into_iter()
                .map(|r| r.observed_version)
                .collect(),
            evidence: "update".into(),
        }],
    )
    .unwrap();
    let gc = s.gc_recovery_images(2).unwrap();
    assert!(gc.removed_images > 0);
    assert_eq!(
        s.publish(&op, vec![mutation(original)]).unwrap().generation,
        1
    );
    // Test fixture loss of the derived ledger; immutable source and decisions remain.
    let db = rusqlite::Connection::open(local.join("journal.sqlite")).unwrap();
    db.execute("DELETE FROM publications", []).unwrap();
    drop(db);
    assert!(matches!(
        s.published_reader().read_published_changes(&basis, 100),
        Err(Error::CoverageLost)
    ));
    assert_eq!(s.rebuild_control_ledger().unwrap().publications, 2);
    assert_eq!(
        s.published_reader()
            .read_published_changes(&basis, 100)
            .unwrap()
            .changes
            .len(),
        2
    );
    assert_eq!(s.read_current(ID).unwrap().heads()[0].body, "second");
}
#[test]
fn fine_grained_publication_allows_unrelated_but_fences_new_dependents() {
    use agentlaw_storage::published::PublishedChangeSource;
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    s.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![mutation(unit(ID, "target"))],
    )
    .unwrap();
    let refs = s
        .read_current(ID)
        .unwrap()
        .references()
        .unwrap()
        .into_iter()
        .map(|r| r.observed_version)
        .collect::<Vec<_>>();
    let baseline = ReadSet {
        source_position: s.published_reader().position().unwrap(),
        observed: std::collections::BTreeMap::from([(ID.into(), refs.clone())]),
        absent: vec![],
        reverse_required: std::collections::BTreeMap::from([(ID.into(), vec![])]),
        watched_scopes: vec![],
    };
    let unrelated = uuid::Uuid::new_v4().to_string();
    let mut u = unit(&unrelated, "unrelated");
    if let UnitState::Live { heads } = &mut u.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    s.publish(&uuid::Uuid::new_v4().to_string(), vec![mutation(u)])
        .unwrap();
    let new_id = uuid::Uuid::new_v4().to_string();
    let mut u = unit(&new_id, "new");
    if let UnitState::Live { heads } = &mut u.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    s.publish_with_readset(
        &uuid::Uuid::new_v4().to_string(),
        vec![mutation(u)],
        &baseline,
        None,
    )
    .unwrap();
    let dependent = uuid::Uuid::new_v4().to_string();
    let mut d = unit(&dependent, "dependent");
    if let UnitState::Live { heads } = &mut d.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
        heads[0].metadata["relations"] = json!([{"kind":"required","target_memory_id":ID}]);
    }
    s.publish(&uuid::Uuid::new_v4().to_string(), vec![mutation(d)])
        .unwrap();
    let mut update = unit(ID, "review result");
    if let UnitState::Live { heads } = &mut update.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    assert!(matches!(
        s.publish_with_readset(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit: update,
                expected_versions: refs,
                evidence: "review".into()
            }],
            &baseline,
            None
        ),
        Err(Error::Stale(_))
    ));
    assert_eq!(s.read_current(ID).unwrap().heads()[0].body, "target");
}
fn unit(id: &str, body: &str) -> CurrentUnit {
    CurrentUnit {
        entity_id: id.into(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":"22222222-2222-4222-8222-222222222222","applicability":{"scope":"user"},"origin":{"machine_id":"33333333-3333-4333-8333-333333333333"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: body.into(),
            }],
        },
    }
}
fn mutation(u: CurrentUnit) -> Mutation {
    Mutation {
        unit: u,
        expected_versions: vec![],
        evidence: "정확한 evidence\r\nno trim".into(),
    }
}
#[test]
fn golden_version_and_embedded_footer() {
    let u = unit(ID, "# Note\n\n<!-- /agentlaw-record-v1 -->\nDone.\n");
    assert_eq!(
        u.references().unwrap()[0].observed_version,
        "av1.IiIiIiIiQiKCIiIiIiIiIs-JP2zTGycw0vghQEmVTbhjP3T3CDOQYPsgJ2EoWWYW"
    );
    let bytes = u.encode().unwrap();
    assert_eq!(CurrentUnit::decode(&mut Cursor::new(bytes)).unwrap(), u);
}
#[test]
fn malformed_and_trailing_bytes_rejected() {
    let u = unit(ID, "한글 😀\r\n");
    let mut b = u.encode().unwrap();
    b.extend_from_slice(b"garbage");
    assert!(CurrentUnit::decode(&mut Cursor::new(b)).is_err());
    let mut b = u.encode().unwrap();
    let i = b.iter().position(|c| *c == b'{').unwrap();
    b[i] = b'!';
    assert!(CurrentUnit::decode(&mut Cursor::new(b)).is_err());
}
#[test]
fn allheads_exact_read_history_and_retry() {
    let t = tempfile::tempdir().unwrap();
    let s = Store::open(t.path().join("source"), t.path().join("local")).unwrap();
    let u = unit(ID, "한글\r\n<!-- /agentlaw-record-v1 --> no LF");
    let op = uuid::Uuid::new_v4().to_string();
    let r = s.publish(&op, vec![mutation(u.clone())]).unwrap();
    assert_eq!(s.read_current(ID).unwrap(), u);
    assert_eq!(s.history(ID).unwrap()[0].body, u.heads()[0].body);
    assert_eq!(
        s.publish(&op, vec![mutation(u.clone())])
            .unwrap()
            .generation,
        r.generation
    );
    assert_eq!(s.history(ID).unwrap().len(), 1);
    let mut m = mutation(unit(ID, "new"));
    if let UnitState::Live { heads } = &mut m.unit.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    assert!(matches!(
        s.publish(&uuid::Uuid::new_v4().to_string(), vec![m.clone()]),
        Err(Error::Stale(_))
    ));
    m.expected_versions = vec![r.references[0].observed_version.clone()];
    s.publish(&uuid::Uuid::new_v4().to_string(), vec![m])
        .unwrap();
    assert_eq!(s.history(ID).unwrap().len(), 2);
}
#[test]
fn restart_at_every_durable_boundary() {
    for point in [
        FaultPoint::Prepared,
        FaultPoint::DirtyFence,
        FaultPoint::Decision,
        FaultPoint::Installed(0),
        FaultPoint::Installed(1),
        FaultPoint::Published,
        FaultPoint::Journal,
        FaultPoint::Clean,
    ] {
        let t = tempfile::tempdir().unwrap();
        let source = t.path().join("source");
        let local = t.path().join("local");
        let s = Store::open(&source, &local).unwrap();
        let op = uuid::Uuid::new_v4().to_string();
        assert!(s
            .publish_with_fault(&op, vec![mutation(unit(ID, "complete"))], Some(point))
            .is_err());
        drop(s);
        let s = Store::open(&source, &local).unwrap();
        if matches!(point, FaultPoint::Prepared | FaultPoint::DirtyFence) {
            assert!(matches!(s.read_current(ID), Err(Error::NotFound(_))));
        } else {
            assert_eq!(s.read_current(ID).unwrap().heads()[0].body, "complete");
            assert_eq!(s.history(ID).unwrap().len(), 1);
        }
    }
}
#[test]
fn partial_batch_is_never_readable_and_missing_recovery_fails_closed() {
    let t = tempfile::tempdir().unwrap();
    let source = t.path().join("source");
    let local = t.path().join("local");
    let s = Store::open(&source, &local).unwrap();
    let op = uuid::Uuid::new_v4().to_string();
    let mut second = unit("44444444-4444-4444-8444-444444444444", "two");
    if let UnitState::Live { heads } = &mut second.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    s.publish_with_fault(
        &op,
        vec![mutation(unit(ID, "one")), mutation(second)],
        Some(FaultPoint::Installed(0)),
    )
    .unwrap_err();
    assert!(matches!(
        s.read_current(ID),
        Err(Error::RecoveryRequired(_))
    ));
    std::fs::rename(
        local.join("recovery").join(&op).join("manifest"),
        local.join("saved-manifest"),
    )
    .unwrap();
    assert!(Store::open(&source, &local).is_err());
}
#[test]
fn journal_loss_does_not_destroy_current() {
    let t = tempfile::tempdir().unwrap();
    let source = t.path().join("source");
    let local = t.path().join("local");
    let s = Store::open(&source, &local).unwrap();
    s.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![mutation(unit(ID, "source independent"))],
    )
    .unwrap();
    drop(s);
    std::fs::remove_file(local.join("journal.sqlite")).unwrap();
    let s = Store::open(&source, &local).unwrap();
    assert_eq!(
        s.read_current(ID).unwrap().heads()[0].body,
        "source independent"
    );
}

#[test]
fn three_parent_consolidation_preserves_original_owners() {
    let t = tempfile::tempdir().unwrap();
    let s = Store::open(t.path().join("source"), t.path().join("local")).unwrap();
    let ids = [
        ID,
        "44444444-4444-4444-8444-444444444444",
        "55555555-5555-4555-8555-555555555555",
    ];
    let mut before = Vec::new();
    for (id, body) in ids.iter().zip(["one", "second closer", "third"]) {
        let mut u = unit(id, body);
        if let UnitState::Live { heads } = &mut u.state {
            heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
        }
        s.publish(&uuid::Uuid::new_v4().to_string(), vec![mutation(u.clone())])
            .unwrap();
        before.push(u);
    }
    let change = uuid::Uuid::new_v4().to_string();
    let mut destination = unit(ID, "second closer!");
    if let UnitState::Live { heads } = &mut destination.state {
        heads[0].metadata["change_id"] = json!(change);
    }
    let mut batch = vec![Mutation {
        unit: destination,
        expected_versions: before[0]
            .references()
            .unwrap()
            .into_iter()
            .map(|r| r.observed_version)
            .collect(),
        evidence: "integrated all causes".into(),
    }];
    for source in &before[1..] {
        batch.push(Mutation {
            unit: CurrentUnit {
                entity_id: source.entity_id.clone(),
                entity_type: "memory".into(),
                state: UnitState::Redirect {
                    redirect_to: ID.into(),
                    consolidation_change_id: change.clone(),
                },
            },
            expected_versions: source
                .references()
                .unwrap()
                .into_iter()
                .map(|r| r.observed_version)
                .collect(),
            evidence: "integration redirect".into(),
        });
    }
    s.publish(&uuid::Uuid::new_v4().to_string(), batch).unwrap();
    assert_eq!(s.resolve_current(ids[2]).unwrap().current.entity_id, ID);
    assert_eq!(s.history(ids[1]).unwrap()[0].body, "second closer");
    let h = s.history(ID).unwrap();
    let last = h.last().unwrap();
    assert_eq!(
        last.metadata["parent_change_ids"].as_array().unwrap().len(),
        3
    );
    assert_eq!(last.metadata["parent_owners"].as_array().unwrap().len(), 3);
    assert_eq!(
        last.metadata["delta_base_id"],
        before[1].heads()[0].metadata["change_id"]
    );
}

#[test]
fn utf8_chunks_cross_pack_and_exact_read() {
    let t = tempfile::tempdir().unwrap();
    let s = Store::open(t.path().join("source"), t.path().join("local")).unwrap();
    let body = "가😀\r\n".repeat(750_000);
    let u = unit(ID, &body);
    let receipt = s
        .publish(&uuid::Uuid::new_v4().to_string(), vec![mutation(u)])
        .unwrap();
    let got = s
        .read_exact(ID, &receipt.references[0].observed_version)
        .unwrap();
    assert_eq!(got.body, body);
    for writer in std::fs::read_dir(t.path().join("source/history")).unwrap() {
        for pack in std::fs::read_dir(writer.unwrap().path()).unwrap() {
            assert!(pack.unwrap().metadata().unwrap().len() <= 16 * 1024 * 1024);
        }
    }
}

#[test]
fn history_torn_reserved_suffix_redoes_without_duplicate() {
    use std::io::{Seek, SeekFrom, Write};
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("source");
    let local = t.path().join("local");
    let s = Store::open(&root, &local).unwrap();
    let first = s
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(unit(ID, "before"))],
        )
        .unwrap();
    let mut next = unit(ID, "after");
    if let UnitState::Live { heads } = &mut next.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    let op = uuid::Uuid::new_v4().to_string();
    s.publish_with_fault(
        &op,
        vec![Mutation {
            unit: next,
            expected_versions: vec![first.references[0].observed_version.clone()],
            evidence: "new evidence".into(),
        }],
        Some(FaultPoint::Decision),
    )
    .unwrap_err();
    let env: serde_json::Value = serde_json::from_slice(
        &std::fs::read(local.join("recovery").join(&op).join("manifest")).unwrap(),
    )
    .unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(env["payload"].as_str().unwrap()).unwrap();
    let target = manifest["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| !t["append"].is_null())
        .unwrap();
    let offset = target["append"]["offset"].as_u64().unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(root.join(target["path"].as_str().unwrap()))
        .unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(b"torn suffix").unwrap();
    file.set_len(offset + 11).unwrap();
    file.sync_all().unwrap();
    drop(file);
    drop(s);
    let s = Store::open(&root, &local).unwrap();
    assert_eq!(s.read_current(ID).unwrap().heads()[0].body, "after");
    assert_eq!(s.history(ID).unwrap().len(), 2);
}

#[test]
fn generation_recheck_prevents_new_dependency_and_retry_reuses_receipt() {
    let t = tempfile::tempdir().unwrap();
    let s = Store::open(t.path().join("source"), t.path().join("local")).unwrap();
    let op = uuid::Uuid::new_v4().to_string();
    let m = mutation(unit(ID, "one"));
    let r = s.publish_if_generation(&op, vec![m.clone()], 0).unwrap();
    assert_eq!(
        s.publish_if_generation(&op, vec![m], 0).unwrap().generation,
        r.generation
    );
    let mut other = unit("44444444-4444-4444-8444-444444444444", "dependent");
    if let UnitState::Live { heads } = &mut other.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    assert!(matches!(
        s.publish_if_generation(&uuid::Uuid::new_v4().to_string(), vec![mutation(other)], 0),
        Err(Error::Stale(_))
    ));
}

#[test]
fn duplicate_json_keys_are_not_silently_accepted() {
    assert!(codec::parse_json(br#"{"x":1,"x":2}"#).is_err());
    assert!(codec::parse_json(br#"{"x":1.0}"#).is_err());
}
#[test]
fn same_source_cannot_use_independent_local_fences() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("source");
    Store::open(&root, t.path().join("local-a")).unwrap();
    assert!(matches!(
        Store::attach_existing(&root, t.path().join("local-b")),
        Err(Error::LocalBindingMismatch)
    ));
}
#[test]
fn attach_foreign_directory_requires_format() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("foreign");
    std::fs::create_dir(&root).unwrap();
    assert!(Store::attach_existing(&root, t.path().join("local")).is_err());
}
#[test]
fn readonly_published_port_reports_coverage_loss_instead_of_empty() {
    use agentlaw_storage::published::PublishedChangeSource;
    let t = tempfile::tempdir().unwrap();
    let local = t.path().join("local");
    let s = Store::open(t.path().join("source"), &local).unwrap();
    let reader = s.published_reader();
    let start = reader.position().unwrap();
    s.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![mutation(unit(ID, "durable change"))],
    )
    .unwrap();
    let batch = reader.read_published_changes(&start, 32).unwrap();
    assert_eq!(batch.changes.len(), 1);
    assert_eq!(
        batch.changes[0].current_units[0].heads()[0].body,
        "durable change"
    );
    assert_eq!(batch.covered_through.sequence, 1);
    std::fs::remove_file(local.join("journal.sqlite")).unwrap();
    assert!(matches!(
        reader.read_published_changes(&start, 32),
        Err(Error::CoverageLost)
    ));
    assert_eq!(
        s.read_current(ID).unwrap().heads()[0].body,
        "durable change"
    );
}
#[test]
fn invalid_applicability_is_not_coerced() {
    let mut u = unit(ID, "body");
    if let UnitState::Live { heads } = &mut u.state {
        heads[0].metadata["applicability"] = json!({"scope":"user","project_id":ID});
    }
    assert!(u.encode().is_err());
    if let UnitState::Live { heads } = &mut u.state {
        heads[0].metadata["applicability"] = json!({"scope":"project"});
    }
    assert!(u.encode().is_err());
}
#[test]
fn streamed_current_exceeds_owned_budget_without_whole_body_allocation() {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("source");
    let s = Store::open(&root, t.path().join("local")).unwrap();
    let u = unit(ID, "");
    let frames = codec::read("current", &mut Cursor::new(u.encode().unwrap())).unwrap();
    let state = &frames[0];
    let chunk = b"\xED\x95\x9C\xEA\xB8\x80\r\n";
    let repeats = 8_500_000u64;
    let bytes = repeats * chunk.len() as u64;
    let mut hash = Sha256::new();
    for _ in 0..repeats {
        hash.update(chunk);
    }
    let digest = format!("{:x}", hash.finalize());
    std::fs::create_dir_all(root.join("current/memory/11")).unwrap();
    let mut f = std::io::BufWriter::new(
        std::fs::File::create(root.join(format!("current/memory/11/{ID}.md"))).unwrap(),
    );
    writeln!(f, "<!-- agentlaw-file-v1 kind=current -->").unwrap();
    writeln!(
        f,
        "<!-- agentlaw-record-v1 type=state key={ID} bytes={} sha256={} -->",
        state.payload.len(),
        codec::digest(&state.payload)
    )
    .unwrap();
    f.write_all(&state.payload).unwrap();
    f.write_all(b"\n<!-- /agentlaw-record-v1 -->\n").unwrap();
    writeln!(f,"<!-- agentlaw-record-v1 type=body key=22222222-2222-4222-8222-222222222222 bytes={bytes} sha256={digest} -->").unwrap();
    for _ in 0..repeats {
        f.write_all(chunk).unwrap();
    }
    f.write_all(b"\n<!-- /agentlaw-record-v1 -->\n<!-- /agentlaw-file-v1 records=2 -->\n")
        .unwrap();
    f.flush().unwrap();
    drop(f);
    assert!(matches!(s.read_current(ID), Err(Error::Capacity)));
    let acquired = s.acquire_current(ID).unwrap();
    let body = acquired.bodies.values().next().unwrap();
    assert_eq!(body.bytes, bytes);
    assert_eq!(body.copy_to(&mut std::io::sink()).unwrap(), bytes);
    assert_eq!(body.sha256, digest);
    assert_eq!(acquired.references.len(), 1);
}
#[test]
fn disk_history_spool_validates_and_visits_causally() {
    let t = tempfile::tempdir().unwrap();
    let s = Store::open(t.path().join("source"), t.path().join("local")).unwrap();
    let first = s
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(unit(ID, "한글\r\noriginal"))],
        )
        .unwrap();
    let mut newer = unit(ID, "한글\r\nnew ✨");
    if let UnitState::Live { heads } = &mut newer.state {
        heads[0].metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    s.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![Mutation {
            unit: newer,
            expected_versions: vec![first.references[0].observed_version.clone()],
            evidence: "second".into(),
        }],
    )
    .unwrap();
    let audit = s.audit_source().unwrap();
    assert_eq!(audit.changes, 2);
    assert_eq!(audit.current_units, 1);
    let mut bodies = Vec::new();
    assert_eq!(
        s.visit_history_closure(ID, |record| {
            bodies.push(record.into_owned()?.body);
            Ok(())
        })
        .unwrap(),
        2
    );
    assert_eq!(bodies, ["한글\r\noriginal", "한글\r\nnew ✨"]);
}
#[test]
fn audit_rejects_current_body_without_matching_history() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("source");
    let s = Store::open(&root, t.path().join("local")).unwrap();
    s.publish(
        &uuid::Uuid::new_v4().to_string(),
        vec![mutation(unit(ID, "original"))],
    )
    .unwrap();
    std::fs::write(
        root.join(format!("current/memory/11/{ID}.md")),
        unit(ID, "external edit").encode().unwrap(),
    )
    .unwrap();
    assert!(s.audit_source().is_err());
}
#[test]
fn cancellation_before_decision_keeps_source_unmodified() {
    use std::sync::atomic::AtomicBool;
    let t = tempfile::tempdir().unwrap();
    let s = Store::open(t.path().join("source"), t.path().join("local")).unwrap();
    assert!(matches!(
        s.publish_if_generation_control(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(unit(ID, "cancel"))],
            0,
            &AtomicBool::new(true)
        ),
        Err(Error::CancelledBeforeDecision)
    ));
    assert!(matches!(s.read_current(ID), Err(Error::NotFound(_))));
}

#[test]
fn actual_process_abort_child() {
    if let Ok(path) = std::env::var("AGENTLAW_STORAGE_ABORT_TEST_ROOT") {
        let root = std::path::PathBuf::from(path);
        let s = Store::open(root.join("source"), root.join("local")).unwrap();
        s.publish_with_fault(
            &uuid::Uuid::new_v4().to_string(),
            vec![mutation(unit(ID, "durably decided"))],
            Some(FaultPoint::Decision),
        )
        .unwrap_err();
        std::process::abort();
    }
}
#[test]
fn actual_aborted_process_recovers_decision() {
    let t = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "actual_process_abort_child", "--nocapture"])
        .env("AGENTLAW_STORAGE_ABORT_TEST_ROOT", t.path())
        .status()
        .unwrap();
    assert!(!status.success());
    let s = Store::open(t.path().join("source"), t.path().join("local")).unwrap();
    assert_eq!(
        s.read_current(ID).unwrap().heads()[0].body,
        "durably decided"
    );
    assert_eq!(s.history(ID).unwrap().len(), 1);
}
