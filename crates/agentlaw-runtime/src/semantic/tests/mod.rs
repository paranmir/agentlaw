use super::*;
use agentlaw_contracts::RequestControl;
use agentlaw_flows::semantic::{
    QueryVector, SemanticCoverage, SemanticError, SemanticPort, SemanticQuery,
};
use agentlaw_search::vector_file::{VectorFileIdentity, VectorFileKind, VectorFileReadView};
use agentlaw_search::ScopeFilter;
use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

const ID: &str = "11111111-1111-4111-8111-111111111111";

mod feeder;
mod history;
mod lifecycle;
mod preparation;
mod quarantine;
mod search;
mod state;
mod writer;

fn mutation(body: &str, expected_versions: Vec<String>) -> Mutation {
    Mutation {
        unit: CurrentUnit {
            entity_id: ID.into(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":"33333333-3333-4333-8333-333333333333"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                    body: body.into(),
                }],
            },
        },
        expected_versions,
        evidence: "semantic fixture".into(),
    }
}

fn test_snapshot(state: agentlaw_worker::WorkerState) -> crate::model::ModelSnapshot {
    crate::model::ModelSnapshot {
        state,
        incarnation: Some("test".into()),
        model_digest: Some("model".into()),
        cause: None,
    }
}

fn drive(indexer: &SemanticIndexer, target: u64) -> usize {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut count = 0;
    loop {
        if let Some(work) = indexer.next_work("model") {
            count += 1;
            indexer.finish(work.id, Ok(vec![1.0, 0.0]));
        }
        if indexer
            .progress("binding")
            .is_some_and(|p| p.complete && p.ack.is_some_and(|a| a.sequence >= target as i64))
        {
            return count;
        }
        assert!(
            Instant::now() < deadline,
            "semantic pump timed out: {:?}",
            indexer.progress("binding")
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_work(indexer: &SemanticIndexer) -> IndexWork {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(work) = indexer.next_work("model") {
            return work;
        }
        assert!(
            Instant::now() < deadline,
            "waiting for model work: {:?}",
            indexer.progress("binding")
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn race_after_final_check(redirect: bool) {
    use std::sync::atomic::AtomicBool;
    use std::sync::Barrier;
    const DEST: &str = "55555555-5555-4555-8555-555555555555";
    let source_id = if redirect { DEST } else { ID };
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("local")).unwrap();
    let mut base_mutation = mutation("base", vec![]);
    base_mutation.unit.entity_id = source_id.into();
    let base = store
        .publish(&uuid::Uuid::new_v4().to_string(), vec![base_mutation])
        .unwrap();
    let destination = if redirect {
        let unit = mutation("destination", vec![]);
        Some(
            store
                .publish(&uuid::Uuid::new_v4().to_string(), vec![unit])
                .unwrap(),
        )
    } else {
        None
    };
    let initial_position = destination
        .as_ref()
        .map_or(base.generation, |r| r.generation);
    let mut config = SemanticConfig::new(temp.path().join("indices"), "config".into());
    config.dimension = 2;
    let indexer = SemanticIndexer::start(
        config.clone(),
        ModelRequestIds::new(),
        HealthSink::default(),
    )
    .unwrap();
    indexer
        .open_binding("binding", store.owned_published_reader())
        .unwrap();
    indexer.model_ready("model");
    drive(&indexer, initial_position);
    let mut stale_mutation = mutation("stale", vec![base.references[0].observed_version.clone()]);
    stale_mutation.unit.entity_id = source_id.into();
    let stale = store
        .publish(&uuid::Uuid::new_v4().to_string(), vec![stale_mutation])
        .unwrap();
    indexer.wake("binding");
    let work = wait_work(&indexer);
    let barrier = Arc::new(Barrier::new(2));
    let hook_barrier = barrier.clone();
    let once = Arc::new(AtomicBool::new(false));
    let hook_once = once.clone();
    *indexer.inner.post_check_hook.lock().unwrap() = Some(Arc::new(move || {
        if !hook_once.swap(true, Ordering::SeqCst) {
            hook_barrier.wait();
            hook_barrier.wait();
        }
    }));
    indexer.finish(work.id, Ok(vec![1.0, 0.0]));
    barrier.wait();
    let latest = if let Some(destination) = destination {
        let change = uuid::Uuid::new_v4().to_string();
        let mut updated_destination = mutation(
            "merged destination",
            vec![destination.references[0].observed_version.clone()],
        );
        updated_destination.unit.entity_id = ID.into();
        if let UnitState::Live { heads } = &mut updated_destination.unit.state {
            heads[0].metadata["change_id"] = json!(change);
        }
        let deleted = Mutation {
            unit: CurrentUnit {
                entity_id: source_id.into(),
                entity_type: "memory".into(),
                state: UnitState::Redirect {
                    redirect_to: ID.into(),
                    consolidation_change_id: change,
                },
            },
            expected_versions: vec![stale.references[0].observed_version.clone()],
            evidence: "integration redirect".into(),
        };
        store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![updated_destination, deleted],
            )
            .unwrap()
    } else {
        store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![mutation(
                    "latest",
                    vec![stale.references[0].observed_version.clone()],
                )],
            )
            .unwrap()
    };
    indexer.wake("binding");
    assert!(indexer.progress("binding").unwrap().ack.unwrap().sequence < latest.generation as i64);
    barrier.wait();
    let next = wait_work(&indexer);
    let path = std::fs::read_dir(&config.indices_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("vector.sqlite");
    let db =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let before_ack: Option<i64> = db
        .query_row("SELECT ack FROM binding_vector_meta", [], |r| r.get(0))
        .unwrap();
    assert!(before_ack.unwrap() < latest.generation as i64);
    let stale_count: i64 = db
        .query_row(
            "SELECT count(*) FROM vectors WHERE change_id=?1",
            [&stale.references[0].observed_version],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stale_count, 1,
        "the barrier must exercise the post-check race"
    );
    drop(db);
    indexer.finish(next.id, Ok(vec![1.0, 0.0]));
    drive(&indexer, latest.generation);
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    for table in ["vectors", "vector_buckets"] {
        let count: i64 = db
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE change_id=?1"),
                [&stale.references[0].observed_version],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
    let source_rows: i64 = db
        .query_row(
            "SELECT count(*) FROM vectors WHERE memory=?1",
            [source_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(source_rows, if redirect { 0 } else { 1 });
    if redirect {
        let source_buckets: i64 = db
            .query_row(
                "SELECT count(*) FROM vector_buckets WHERE memory=?1",
                [source_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source_buckets, 0);
    } else {
        let latest_version = &latest
            .references
            .iter()
            .find(|r| r.memory_id == source_id)
            .unwrap()
            .observed_version;
        let stored: String = db
            .query_row(
                "SELECT change_id FROM vectors WHERE memory=?1",
                [source_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(&stored, latest_version);
    }
    drop(db);
    assert!(indexer.shutdown(Duration::from_secs(5)));
}
