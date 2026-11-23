use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
    static THRESHOLD: Cell<usize> = const { Cell::new(1024 * 1024) };
}
struct MeasuringAllocator;
#[global_allocator]
static ALLOCATOR: MeasuringAllocator = MeasuringAllocator;
fn record(size: usize, sign: isize) {
    if size < THRESHOLD.try_with(Cell::get).unwrap_or(usize::MAX)
        || !TRACK.try_with(Cell::get).unwrap_or(false)
    {
        return;
    }
    let _ = LIVE.try_with(|live| {
        let value = live.get() + sign * size as isize;
        live.set(value);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(value)));
    });
}
pub(super) fn measure<R>(threshold: usize, f: impl FnOnce() -> R) -> (R, isize) {
    THRESHOLD.with(|value| value.set(threshold));
    LIVE.with(|value| value.set(0));
    PEAK.with(|value| value.set(0));
    TRACK.with(|value| value.set(true));
    let result = f();
    TRACK.with(|value| value.set(false));
    (result, PEAK.with(Cell::get))
}
unsafe impl GlobalAlloc for MeasuringAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            record(layout.size(), 1);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(layout);
        if !p.is_null() {
            record(layout.size(), 1);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        record(layout.size(), -1);
        System.dealloc(p, layout);
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = System.realloc(p, layout, size);
        if !result.is_null() {
            record(layout.size(), -1);
            record(size, 1);
        }
        result
    }
}

#[test]
fn snapshot_one_body_peak_is_at_most_budget_plus_one_body() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        agentlaw_storage::Store::open(dir.path().join("source"), dir.path().join("canonical"))
            .unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let exact_file = dir.path().join("exact-memory.sqlite");
    let lexical_file = dir.path().join("lexical-memory.sqlite");
    let index = ExactIndex::open(&exact_file).unwrap();
    index
        .connection
        .execute(
            "INSERT INTO exact_position VALUES(1,?1,?2)",
            params![position.epoch, position.sequence.to_string()],
        )
        .unwrap();
    index
        .connection
        .execute(
            "INSERT INTO exact_lexical VALUES(1,'lexical-memory.sqlite')",
            [],
        )
        .unwrap();
    let _lexical = agentlaw_search::SearchIndex::open(&lexical_file).unwrap();
    let registry =
        ProjectionGenerations::open("memory", &position.epoch, &exact_file, &lexical_file).unwrap();
    let unit = CurrentUnit {
        entity_id: uuid::Uuid::new_v4().to_string(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![agentlaw_storage::Head {
                metadata: serde_json::json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: "x".repeat(8 * 1024 * 1024),
            }],
        },
    };
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![agentlaw_storage::Mutation {
                unit,
                expected_versions: vec![],
                evidence: "fixture".into(),
            }],
        )
        .unwrap();
    let pinned = registry.pin().unwrap();
    let guard = source.read_snapshot().unwrap();
    LIVE.with(|v| v.set(0));
    PEAK.with(|v| v.set(0));
    TRACK.with(|v| v.set(true));
    let result = ExactSnapshot::build(
        pinned,
        &guard,
        DeltaBudget {
            max_identities: 1,
            max_resident_bytes: 8 * 1024 * 1024,
        },
    );
    TRACK.with(|v| v.set(false));
    let peak = PEAK.with(Cell::get);
    eprintln!("snapshot large-allocation peak: {peak} bytes");
    assert!(result.is_ok(), "snapshot failed");
    assert!(
        peak <= 16 * 1024 * 1024,
        "large-allocation peak {peak} exceeds 16 MiB"
    );
}

#[test]
fn multihead_over_budget_does_not_load_the_remaining_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        agentlaw_storage::Store::open(dir.path().join("source"), dir.path().join("canonical"))
            .unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let exact = dir.path().join("exact-multi.sqlite");
    let lexical = dir.path().join("lexical-multi.sqlite");
    let index = ExactIndex::open(&exact).unwrap();
    index
        .connection
        .execute(
            "INSERT INTO exact_position VALUES(1,?1,?2)",
            params![position.epoch, position.sequence.to_string()],
        )
        .unwrap();
    index
        .connection
        .execute(
            "INSERT INTO exact_lexical VALUES(1,'lexical-multi.sqlite')",
            [],
        )
        .unwrap();
    let _lexical = agentlaw_search::SearchIndex::open(&lexical).unwrap();
    let registry = ProjectionGenerations::open("multi", &position.epoch, &exact, &lexical).unwrap();
    let mut unit = CurrentUnit {
        entity_id: uuid::Uuid::new_v4().to_string(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![agentlaw_storage::Head {
                metadata: serde_json::json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: "x".repeat(4 * 1024 * 1024),
            }],
        },
    };
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![agentlaw_storage::Mutation {
                unit: unit.clone(),
                expected_versions: vec![],
                evidence: "fixture".into(),
            }],
        )
        .unwrap();
    if let UnitState::Live { heads } = &mut unit.state {
        for _ in 0..3 {
            let mut next = heads[0].clone();
            next.metadata["change_id"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
            heads.push(next);
        }
    }
    let path = dir.path().join(format!(
        "source/current/memory/{}/{}.md",
        &unit.entity_id[..2],
        unit.entity_id
    ));
    std::fs::write(path, unit.encode().unwrap()).unwrap();
    drop(unit);
    let pinned = registry.pin().unwrap();
    let guard = source.read_snapshot().unwrap();
    let (result, peak) = measure(1024 * 1024, || {
        ExactSnapshot::build(
            pinned,
            &guard,
            DeltaBudget {
                max_identities: 1,
                max_resident_bytes: 4 * 1024 * 1024,
            },
        )
    });
    assert_eq!(result.err().unwrap().code, "capacity");
    assert!(
        peak <= 8 * 1024 * 1024,
        "multihead large-allocation peak {peak}"
    );
}
