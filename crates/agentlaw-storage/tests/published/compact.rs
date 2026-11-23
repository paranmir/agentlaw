use agentlaw_storage::{codec, CurrentUnit, Error, Head, Store, UnitState};
use serde_json::json;
use std::io::Cursor;

use super::ID;
const CHANGE: &str = "22222222-2222-4222-8222-222222222222";
fn unit() -> CurrentUnit {
    CurrentUnit {
        entity_id: ID.into(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                metadata: json!({"change_id":CHANGE,"applicability":{"scope":"user"},"origin":{"machine_id":"33333333-3333-4333-8333-333333333333"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: "complete body 한글\r\n".into(),
            }],
        },
    }
}
fn frames(u: &CurrentUnit) -> Vec<codec::Frame> {
    codec::read("current", &mut Cursor::new(u.encode().unwrap())).unwrap()
}
fn encode(frames: &[codec::Frame]) -> Vec<u8> {
    let mut bytes = Vec::new();
    codec::write("current", frames, &mut bytes).unwrap();
    bytes
}
fn kind(e: &Error) -> String {
    match e {
        Error::Corrupt(_) => "Corrupt".into(),
        Error::Io(e) => format!("Io({:?})", e.kind()),
        Error::Capacity => "Capacity".into(),
        other => format!("{other:?}"),
    }
}
fn compare_case(bytes: &[u8]) -> (String, String) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("source"), dir.path().join("canonical")).unwrap();
    let path = dir.path().join(format!("source/current/memory/11/{ID}.md"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    let reader = store.owned_published_reader();
    let guard = reader.read_snapshot().unwrap();
    let old = guard.read_current(ID);
    let new = guard.read_current_compact(ID, false, u64::MAX);
    let left = old
        .as_ref()
        .map(|_| "Ok".to_owned())
        .unwrap_or_else(|e| kind(e));
    let right = new
        .as_ref()
        .map(|_| "Ok".to_owned())
        .unwrap_or_else(|e| kind(e));
    eprintln!("COMPACT_COMPARE old={old:?} new={new:?}");
    if let (Ok(a), Ok(b)) = (old, new) {
        assert_eq!(a, b);
        assert_eq!(a.references().unwrap(), b.references().unwrap());
    }
    (left, right)
}
#[test]
fn compact_matches_legacy_on_valid_units_and_semantic_failures() {
    let u = unit();
    let mut cases = vec![("valid", u.encode().unwrap())];
    let mut multi = u.clone();
    if let UnitState::Live { heads } = &mut multi.state {
        let mut next = heads[0].clone();
        next.metadata["change_id"] = json!(uuid::Uuid::new_v4().to_string());
        next.body = "second 🦀 head".into();
        heads.push(next);
    }
    cases.push(("multihead", multi.encode().unwrap()));
    let mut reversed = frames(&multi);
    reversed.reverse();
    cases.push(("frame_order", encode(&reversed)));
    let mut f = frames(&u);
    let mut state: serde_json::Value = serde_json::from_slice(&f[0].payload).unwrap();
    state["current_heads"][0]["is_rule"] = json!("invalid");
    f[0].payload = serde_json::to_vec(&state).unwrap();
    cases.push(("bad_rule", encode(&f)));
    let mut f = frames(&u);
    let mut state: serde_json::Value = serde_json::from_slice(&f[0].payload).unwrap();
    state["current_heads"][0]["applicability"] = json!({"scope":"project","project_id":"invalid"});
    f[0].payload = serde_json::to_vec(&state).unwrap();
    cases.push(("bad_scope", encode(&f)));
    let mut f = frames(&u);
    f.pop();
    cases.push(("missing_body", encode(&f)));
    let mut f = frames(&u);
    f[0].key = uuid::Uuid::new_v4().to_string();
    cases.push(("state_key", encode(&f)));
    let mut bytes = u.encode().unwrap();
    bytes.extend_from_slice(b"trailing");
    cases.push(("trailing", bytes));
    for (name, bytes) in cases {
        let (a, b) = compare_case(&bytes);
        assert_eq!(a, b, "{name}");
    }
}
#[test]
fn compact_preserves_corruption_class_for_invalid_utf8() {
    let mut f = frames(&unit());
    f[1].payload = vec![0xff];
    let (a, b) = compare_case(&encode(&f));
    assert_eq!(a, b);
}
#[test]
fn compact_preserves_corruption_class_for_truncated_body() {
    let body = b"complete body";
    let mut bytes = unit().encode().unwrap();
    let start = bytes.windows(body.len()).position(|w| w == body).unwrap();
    bytes.truncate(start + 5);
    let (a, b) = compare_case(&bytes);
    assert_eq!(a, b);
}
#[test]
fn compact_preserves_checksum_error_precedence_over_footer_eof() {
    let body = b"complete body";
    let mut bytes = unit().encode().unwrap();
    let start = bytes.windows(body.len()).position(|w| w == body).unwrap();
    bytes[start] = 0xff;
    bytes.truncate(start + 5);
    let (a, b) = compare_case(&bytes);
    assert_eq!(a, b);
}
