use super::*;
fn uid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn live(id: &str, machine: &str, body: &str) -> CurrentUnit {
    CurrentUnit {
        entity_id: id.into(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":uid(),"applicability":{"scope":"user"},"origin":{"machine_id":machine,"user_id":"test"},"recorded_at_ms":1,"is_rule":false,"relations":[],"work_targets":[]}),
                body: body.into(),
            }],
        },
    }
}
fn write(store: &Store, unit: CurrentUnit) {
    let expected = store
        .read_current(&unit.entity_id)
        .ok()
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
            &uid(),
            vec![Mutation {
                unit,
                expected_versions: expected,
                evidence: "isolated storage sync fixture".into(),
            }],
        )
        .unwrap();
}
fn capture(store: &Store, parent: &Path, name: &str) -> (Store, Capture) {
    let root = parent.join(name);
    let captured = store
        .capture_sync(&root, &CaptureLimits::default(), None)
        .unwrap();
    let snapshot = Store::attach_existing_with_coordination(
        root,
        parent.join(format!("{name}-control")),
        store.coordination_root(),
    )
    .unwrap();
    (snapshot, captured)
}
#[test]
fn bounded_capture_has_no_pending_or_control_state() {
    let t = tempfile::tempdir().unwrap();
    let store = Store::open(t.path().join("source"), t.path().join("control")).unwrap();
    write(&store, live(&uid(), &uid(), "saved task/progress"));
    fs::write(store.local.join("pending-private.json"), b"never shared").unwrap();
    let (snapshot, receipt) = capture(&store, t.path(), "fixed");
    assert!(receipt
        .files
        .keys()
        .all(|p| !p.contains("pending") && !p.contains("control")));
    assert_eq!(snapshot.snapshot().unwrap().1.len(), 1);
    let target = t.path().join("too-large");
    let limits = CaptureLimits {
        bytes: 1,
        files: 1,
        elapsed: Duration::from_secs(5),
    };
    assert!(store.capture_sync(&target, &limits, None).is_err());
    assert!(!target.with_extension("capture.json").exists());
    assert_eq!(store.snapshot().unwrap().1.len(), 1);
}
fn changed_overlay<'a>(t: &'a tempfile::TempDir) -> (Store, Store, Store, Capture, String, String) {
    let source = Store::open(t.path().join("active"), t.path().join("active-control")).unwrap();
    let id = uid();
    let machine = uid();
    write(&source, live(&id, &machine, "base"));
    let (baseline, receipt) = capture(&source, t.path(), "baseline");
    let (incoming, _) = capture(&source, t.path(), "incoming");
    write(&incoming, live(&id, &machine, "incoming descends base"));
    let resolved = baseline
        .prepare_sync_union(
            &incoming,
            t.path().join("resolved").as_path(),
            t.path().join("resolved-control").as_path(),
        )
        .unwrap();
    (source, baseline, resolved, receipt, id, machine)
}
#[test]
fn same_history_shard_later_append_is_preserved() {
    let t = tempfile::tempdir().unwrap();
    let (source, baseline, resolved, receipt, id, machine) = changed_overlay(&t);
    let guard = source
        .prepare_overlay_guard(&receipt, &baseline, &resolved)
        .unwrap();
    let other = uid();
    write(&source, live(&other, &machine, "unrelated later write"));
    source
        .publish_sync_overlay(&uid(), &baseline, &resolved, &guard, None)
        .unwrap();
    assert_eq!(
        source.read_current(&other).unwrap().heads()[0].body,
        "unrelated later write"
    );
    assert_eq!(
        source.read_current(&id).unwrap().heads()[0].body,
        "incoming descends base"
    );
    assert_eq!(source.history(&other).unwrap().len(), 1);
    source.audit_source().unwrap();
}
#[test]
fn new_required_dependent_and_aba_invalidate_only_overlay() {
    let t = tempfile::tempdir().unwrap();
    let (source, baseline, resolved, receipt, id, machine) = changed_overlay(&t);
    let guard = source
        .prepare_overlay_guard(&receipt, &baseline, &resolved)
        .unwrap();
    let dep = uid();
    let mut unit = live(&dep, &machine, "new required dependent");
    if let UnitState::Live { heads } = &mut unit.state {
        heads[0].metadata["relations"] = json!([{"kind":"required","target_memory_id":id}]);
    }
    write(&source, unit);
    let result = source.publish_sync_overlay(&uid(), &baseline, &resolved, &guard, None);
    assert!(matches!(result, Err(Error::Stale(_))));
    write(&source, live(&dep, &machine, "edge removed again"));
    assert!(matches!(
        source.publish_sync_overlay(&uid(), &baseline, &resolved, &guard, None),
        Err(Error::Stale(_))
    ));
    assert_eq!(source.read_current(&id).unwrap().heads()[0].body, "base");
}
#[test]
fn reviewed_forward_reference_and_related_dependent_are_guarded() {
    for kind in ["required", "related"] {
        let t = tempfile::tempdir().unwrap();
        let source = Store::open(t.path().join("active"), t.path().join("active-control")).unwrap();
        let (a, b, machine) = (uid(), uid(), uid());
        write(&source, live(&b, &machine, "judgment context"));
        let linked = |body: &str| {
            let mut unit = live(&a, &machine, body);
            if let UnitState::Live { heads } = &mut unit.state {
                heads[0].metadata["relations"] = json!([{"kind":kind,"target_memory_id":b}]);
            }
            unit
        };
        write(&source, linked("base"));
        let (baseline, captured) = capture(&source, t.path(), "baseline");
        let (incoming, _) = capture(&source, t.path(), "incoming");
        write(&incoming, linked("resolved against B"));
        let resolved = baseline
            .prepare_sync_union(
                &incoming,
                &t.path().join("resolved"),
                &t.path().join("resolved-control"),
            )
            .unwrap();
        let guard = source
            .prepare_overlay_guard(&captured, &baseline, &resolved)
            .unwrap();
        write(&source, live(&b, &machine, "B changed after review"));
        assert!(matches!(
            source.publish_sync_overlay(&uid(), &baseline, &resolved, &guard, None),
            Err(Error::Stale(_))
        ));
        assert_eq!(source.read_current(&a).unwrap().heads()[0].body, "base");
    }
}
#[test]
fn decision_fault_recovers_receipt_before_old_inputs() {
    let t = tempfile::tempdir().unwrap();
    let (source, baseline, resolved, receipt, id, machine) = changed_overlay(&t);
    let guard = source
        .prepare_overlay_guard(&receipt, &baseline, &resolved)
        .unwrap();
    let operation = uid();
    assert!(matches!(
        source.publish_sync_overlay(
            &operation,
            &baseline,
            &resolved,
            &guard,
            Some(FaultPoint::Decision)
        ),
        Err(Error::Interrupted(FaultPoint::Decision))
    ));
    let reopened = Store::open(&source.root, &source.local).unwrap();
    let first = reopened.sync_receipt(&operation).unwrap().unwrap();
    write(&reopened, live(&id, &machine, "later after decision"));
    let replay = reopened
        .publish_sync_overlay(&operation, &baseline, &resolved, &guard, None)
        .unwrap();
    assert_eq!(first.generation, replay.generation);
    assert_eq!(
        reopened.read_current(&id).unwrap().heads()[0].body,
        "later after decision"
    );
}
#[test]
fn cancelled_before_decision_does_not_apply() {
    let t = tempfile::tempdir().unwrap();
    let (source, baseline, resolved, receipt, id, _) = changed_overlay(&t);
    let guard = source
        .prepare_overlay_guard(&receipt, &baseline, &resolved)
        .unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(true);
    assert!(matches!(
        source.publish_sync_overlay_control(
            &uid(),
            &baseline,
            &resolved,
            &guard,
            None,
            Some(&cancel)
        ),
        Err(Error::CancelledBeforeDecision)
    ));
    assert_eq!(source.read_current(&id).unwrap().heads()[0].body, "base");
}
#[test]
fn completed_operation_rejects_different_request_before_replay() {
    let t = tempfile::tempdir().unwrap();
    let (source, baseline, resolved, receipt, id, _) = changed_overlay(&t);
    let guard = source
        .prepare_overlay_guard(&receipt, &baseline, &resolved)
        .unwrap();
    let operation = uid();
    let first = source
        .publish_sync_overlay(&operation, &baseline, &resolved, &guard, None)
        .unwrap();
    let replay = source
        .publish_sync_overlay(&operation, &baseline, &resolved, &guard, None)
        .unwrap();
    assert_eq!(first.generation, replay.generation);
    let mut different = guard.clone();
    different.paths.clear();
    assert!(
        matches!(source.publish_sync_overlay(&operation, &baseline, &resolved, &different, None), Err(Error::Corrupt(message)) if message.contains("different sync request"))
    );
    assert_eq!(
        source.read_current(&id).unwrap().heads()[0].body,
        "incoming descends base"
    );
}
fn consolidate(store: &Store, source: &str, target: &str, machine: &str, body: &str) {
    let result = live(target, machine, body);
    let change = result.heads()[0].metadata["change_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let old = store.read_current(source).unwrap();
    let dest = store.read_current(target).unwrap();
    store
        .publish(
            &uid(),
            vec![
                Mutation {
                    unit: result,
                    expected_versions: dest
                        .references()
                        .unwrap()
                        .into_iter()
                        .map(|r| r.observed_version)
                        .collect(),
                    evidence: "consolidate retained identities".into(),
                },
                Mutation {
                    unit: CurrentUnit {
                        entity_id: source.into(),
                        entity_type: "memory".into(),
                        state: UnitState::Redirect {
                            redirect_to: target.into(),
                            consolidation_change_id: change,
                        },
                    },
                    expected_versions: old
                        .references()
                        .unwrap()
                        .into_iter()
                        .map(|r| r.observed_version)
                        .collect(),
                    evidence: "redirect consolidation source".into(),
                },
            ],
        )
        .unwrap();
}
#[test]
fn divergent_redirects_keep_both_consolidation_lineages() {
    let t = tempfile::tempdir().unwrap();
    let initial = Store::open(t.path().join("initial"), t.path().join("initial-control")).unwrap();
    let mut ids = vec![uid(), uid(), uid()];
    ids.sort();
    let (b, c, a) = (&ids[0], &ids[1], &ids[2]);
    let machine = uid();
    for id in &ids {
        write(&initial, live(id, &machine, "shared root"));
    }
    let (left, _) = capture(&initial, t.path(), "left");
    let (right, _) = capture(&initial, t.path(), "right");
    consolidate(&left, a, b, &machine, "A to B interpretation");
    consolidate(&right, a, c, &machine, "A to C interpretation");
    let candidate = left
        .prepare_sync_union(
            &right,
            &t.path().join("candidate"),
            &t.path().join("candidate-control"),
        )
        .unwrap();
    let lp = left.read_current(b).unwrap().references().unwrap();
    let rp = right.read_current(c).unwrap().references().unwrap();
    let final_unit = live(
        b,
        &machine,
        "both interpretations retained at B; C still independent",
    );
    let cid = final_unit.heads()[0].metadata["change_id"]
        .as_str()
        .unwrap()
        .to_string();
    let resolution = Resolution {
        units: vec![
            ResolutionUnit {
                unit: final_unit,
                parents: lp.into_iter().chain(rp).collect(),
                evidence: "LLM whole retained graph solution".into(),
            },
            ResolutionUnit {
                unit: CurrentUnit {
                    entity_id: a.clone(),
                    entity_type: "memory".into(),
                    state: UnitState::Redirect {
                        redirect_to: b.clone(),
                        consolidation_change_id: cid,
                    },
                },
                parents: vec![],
                evidence: "final A alias".into(),
            },
        ],
        projects: BTreeMap::new(),
    };
    candidate
        .apply_import_resolution(&uid(), &resolution)
        .unwrap();
    candidate.audit_source().unwrap();
    left.validate_reconciled_source(&candidate).unwrap();
    right.validate_reconciled_source(&candidate).unwrap();
    assert_eq!(candidate.resolve_current(a).unwrap().current.entity_id, *b);
    assert_eq!(
        candidate.read_current(c).unwrap().heads()[0].body,
        "A to C interpretation"
    );
    let history = candidate.history_closure(b).unwrap();
    assert!(history.iter().any(|h| h.body == "A to B interpretation"));
    assert!(history.iter().any(|h| h.body == "A to C interpretation"));
}
