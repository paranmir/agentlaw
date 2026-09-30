use agentlaw_contracts::{group_retrieval_paths, RetrievalPath};
use serde_json::json;

fn path(via: &[&str], clue: &str, source: Option<&str>) -> RetrievalPath {
    RetrievalPath {
        via: via.iter().map(|s| s.to_string()).collect(),
        clue: clue.into(),
        source_memory_id: source.map(str::to_string),
    }
}

#[test]
fn exact_clues_group_channels_without_losing_different_clues_or_sources() {
    let paths = group_retrieval_paths([
        path(&["lexical"], "same clue", None),
        path(&["vector", "lexical"], "same clue", None),
        path(&["vector"], "different clue", None),
        path(&["related_memory"], "same clue", Some("source-a")),
        path(&["related_memory"], "same clue", Some("source-b")),
        path(&["work_target"], "same clue", None),
        path(&["lexical"], "same clue ", None),
        path(&["lexical"], "caf\u{e9}", None),
        path(&["vector"], "cafe\u{301}", None),
    ]);
    let value = serde_json::to_value(&paths).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 7);
    assert_eq!(
        value[0],
        json!({"via":["lexical","vector","work_target"],"clue":"same clue"})
    );
    assert_eq!(value[1]["clue"], "different clue");
    assert_eq!(value[2]["source_memory_id"], "source-a");
    assert_eq!(value[3]["source_memory_id"], "source-b");
    assert_eq!(value[4]["clue"], "same clue ");
    assert_eq!(value[5]["clue"], "caf\u{e9}");
    assert_eq!(value[6]["clue"], "cafe\u{301}");
    assert_eq!(
        serde_json::to_value(group_retrieval_paths(paths)).unwrap(),
        value
    );
}

#[test]
fn grouped_candidate_is_smaller_than_repeated_channel_text() {
    let clue = "Use argument arrays for paths with spaces; preserve the declared scope and source.";
    let mut legacy = json!({
        "memory_id":"11111111-1111-4111-8111-111111111111",
        "excerpt":clue.repeat(5),
        "applicability":{"scope":["user"]},
        "retrieval_paths":[{"via":"lexical","clue":clue},{"via":"vector","clue":clue}]
    });
    let legacy_bytes = serde_json::to_vec(&legacy).unwrap().len();
    legacy["retrieval_paths"] = serde_json::to_value(group_retrieval_paths([
        path(&["lexical"], clue, None),
        path(&["vector"], clue, None),
    ]))
    .unwrap();
    let grouped_bytes = serde_json::to_vec(&legacy).unwrap().len();
    assert!(grouped_bytes < legacy_bytes);
    assert_eq!(legacy["retrieval_paths"].as_array().unwrap().len(), 1);
    assert_eq!(
        legacy["retrieval_paths"][0]["via"],
        json!(["lexical", "vector"])
    );
    assert_eq!(legacy["retrieval_paths"][0]["clue"], clue);
    println!("Representative candidate JSON: {legacy_bytes} -> {grouped_bytes} UTF-8 bytes");
}
