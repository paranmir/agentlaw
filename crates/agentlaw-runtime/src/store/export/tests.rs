use super::*;
use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
use serde_json::{json, Value};
use std::fs;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn filter() -> InventoryFilter {
    InventoryFilter {
        scope: None,
        project: None,
        machine: None,
        table: false,
    }
}

fn entries(path: &Path) -> std::collections::BTreeSet<std::ffi::OsString> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect()
}

#[test]
fn cancelled_exports_leave_no_output_or_scratch_entries() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    fs::create_dir_all(&local).unwrap();
    let _store = Store::open(&root, local.join("canonical")).unwrap();
    let parent = dir.path().join("output");
    fs::create_dir(&parent).unwrap();
    let parent_before = entries(&parent);
    let local_before = entries(&local);
    let control = RequestControl::new(Arc::new(AtomicBool::new(true)), |_| {});
    let history = parent.join("history.jsonl");
    assert_eq!(
        history_export(&root, "missing", &history, &control)
            .unwrap_err()
            .code,
        "cancelled"
    );
    assert!(!history.exists());
    assert_eq!(entries(&parent), parent_before);
    assert_eq!(entries(&local), local_before);
    let inventory = parent.join("inventory.jsonl");
    assert_eq!(
        procedure_list(&root, &local, &filter(), &inventory, &control)
            .unwrap_err()
            .code,
        "cancelled"
    );
    assert!(!inventory.exists());
    assert_eq!(entries(&parent), parent_before);
    assert_eq!(entries(&local), local_before);
}

#[test]
fn cancellation_from_export_progress_leaves_no_output_or_scratch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    let parent = dir.path().join("output");
    fs::create_dir_all(&local).unwrap();
    fs::create_dir(&parent).unwrap();
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    for (kind, entity_id, metadata, body) in [
        (
            "memory",
            id.clone(),
            json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
            "Saved observation.",
        ),
        (
            "learned_procedure",
            uuid::Uuid::new_v4().to_string(),
            json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"name":"Fixture procedure","use_when":"When testing","evidence_memory_ids":[]}),
            "Perform the check.",
        ),
    ] {
        store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![Mutation {
                    unit: CurrentUnit {
                        entity_type: kind.into(),
                        entity_id,
                        state: UnitState::Live {
                            heads: vec![Head {
                                metadata,
                                body: body.into(),
                            }],
                        },
                    },
                    expected_versions: vec![],
                    evidence: "Observed fixture".into(),
                }],
            )
            .unwrap();
    }
    let output_before = entries(&parent);
    let local_before = entries(&local);
    for inventory in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let callback = cancel.clone();
        let control = RequestControl::new(cancel.clone(), move |_| {
            callback.store(true, Ordering::Release)
        });
        let path = parent.join(if inventory {
            "inventory.jsonl"
        } else {
            "history.jsonl"
        });
        let result = if inventory {
            procedure_list(&root, &local, &filter(), &path, &control)
        } else {
            history_export(&root, &id, &path, &control)
        };
        assert!(
            cancel.load(Ordering::Acquire),
            "export never reported progress"
        );
        assert_eq!(result.unwrap_err().code, "cancelled");
        assert!(!path.exists());
        assert_eq!(entries(&parent), output_before);
        assert_eq!(entries(&local), local_before);
    }
}

#[test]
fn export_has_exact_manifest_and_lossless_changes_without_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let store = Store::open(&root, dir.path().join("local")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut expected = vec![];
    for body in ["첫 기억\r\n끝", "첫 기억\r\n고친 끝"] {
        let unit = CurrentUnit {
            entity_id: id.clone(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                    body: body.into(),
                }],
            },
        };
        expected = store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![Mutation {
                    unit,
                    expected_versions: expected,
                    evidence: "Observed correction".into(),
                }],
            )
            .unwrap()
            .references
            .into_iter()
            .map(|r| r.observed_version)
            .collect();
    }
    let output = dir.path().join("audit.jsonl");
    let control = RequestControl::default();
    assert_eq!(
        history_export(&root, &id, &output, &control).unwrap()["change_count"],
        2
    );
    let raw = fs::read_to_string(&output).unwrap();
    let records: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records[0].as_object().unwrap().len(), 7);
    assert_eq!(records[0]["record_type"], "manifest");
    assert_eq!(records[1]["record_type"], "change");
    assert!(records[1].get("delta_base_id").is_none());
    assert_eq!(records[2]["parent_change_ids"][0], records[1]["change_id"]);
    assert!(records[2]["markdown_diff"]
        .as_str()
        .unwrap()
        .contains("고친 끝"));
    assert_eq!(
        history_export(&root, &id, &output, &control)
            .unwrap_err()
            .code,
        "output_exists"
    );
    assert_eq!(fs::read_to_string(&output).unwrap(), raw);
}

#[test]
fn complete_inventory_is_sorted_compact_and_no_clobber() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    fs::create_dir(&local).unwrap();
    let store = Store::open(&root, local.join("canonical")).unwrap();
    let ids = [
        "ffffffff-ffff-4fff-8fff-ffffffffffff",
        "00000000-0000-4000-8000-000000000001",
    ];
    for id in ids {
        store.publish(&uuid::Uuid::new_v4().to_string(), vec![Mutation { unit: CurrentUnit { entity_type: "learned_procedure".into(), entity_id: id.into(), state: UnitState::Live { heads: vec![Head {
            metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"name":"Shell safety","use_when":"Before PowerShell file operations","evidence_memory_ids":[]}), body: "Instructions must not be in inventory output.".into()
        }] } }, expected_versions: vec![], evidence: "test".into() }]).unwrap();
    }
    let output = dir.path().join("inventory.jsonl");
    let control = RequestControl::default();
    assert_eq!(
        procedure_list(&root, &local, &filter(), &output, &control).unwrap()["procedure_count"],
        2
    );
    let text = fs::read_to_string(&output).unwrap();
    let rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows[0]["procedure_id"], ids[1]);
    assert_eq!(rows[0].as_object().unwrap().len(), 4);
    assert!(!text.contains("Instructions must"));
    assert_eq!(
        procedure_list(&root, &local, &filter(), &output, &control)
            .unwrap_err()
            .code,
        "output_exists"
    );
    assert_eq!(fs::read_to_string(&output).unwrap(), text);
}

#[test]
fn export_rejects_relative_and_source_tree_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    fs::create_dir(&root).unwrap();
    let local = dir.path().join("local");
    fs::create_dir(&local).unwrap();
    let control = RequestControl::default();
    for path in [
        std::path::PathBuf::from("relative.jsonl"),
        root.join("output.jsonl"),
    ] {
        assert!(procedure_list(&root, &local, &filter(), &path, &control).is_err());
        assert!(!path.exists());
    }
}

#[test]
fn export_does_not_clobber_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    fs::create_dir(&root).unwrap();
    let local = dir.path().join("local");
    fs::create_dir(&local).unwrap();
    let output = dir.path().join("existing.jsonl");
    fs::write(&output, b"keep").unwrap();
    assert!(procedure_list(
        &root,
        &local,
        &filter(),
        &output,
        &RequestControl::default()
    )
    .is_err());
    assert_eq!(fs::read(&output).unwrap(), b"keep");
}

#[test]
fn export_to_missing_parent_leaves_no_output_or_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    let local = dir.path().join("local");
    fs::create_dir(&local).unwrap();
    let _store = Store::open(&root, local.join("canonical")).unwrap();
    let local_before = entries(&local);
    let before = entries(dir.path());
    let output = dir.path().join("missing").join("output.jsonl");
    assert!(procedure_list(
        &root,
        &local,
        &filter(),
        &output,
        &RequestControl::default()
    )
    .is_err());
    assert!(!output.exists());
    assert!(!output.parent().unwrap().exists());
    let after = entries(dir.path());
    assert_eq!(
        before, after,
        "export left scratch files in the parent directory"
    );
    assert_eq!(
        entries(&local),
        local_before,
        "export left scratch files in the binding local directory"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn export_rejects_alias_into_source_tree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    fs::create_dir(&root).unwrap();
    let local = dir.path().join("local");
    fs::create_dir(&local).unwrap();
    let alias = dir.path().join("alias");
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&root, &alias);
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_dir(&root, &alias);
    if let Err(error) = linked {
        if error.kind() == std::io::ErrorKind::PermissionDenied
            || error.raw_os_error() == Some(1314)
        {
            eprintln!("Skipping link test: creating a directory link requires permission: {error}");
            return;
        }
        panic!("creating directory link failed: {error}");
    }
    let output = alias.join("output.jsonl");
    assert!(procedure_list(
        &root,
        &local,
        &filter(),
        &output,
        &RequestControl::default()
    )
    .is_err());
    assert!(!output.exists());
}
