use super::*;
use agentlaw_storage::{CurrentUnit, Head, Mutation, Store, UnitState};
static LARGE_HISTORY_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn fixture(count: u32) -> (tempfile::TempDir, Store, String, Vec<String>) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("source"), temp.path().join("canonical")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut versions = vec![];
    let mut changes = vec![];
    for n in 0..count {
        let change = uuid::Uuid::new_v4().to_string();
        changes.push(change.clone());
        let body=format!("# Current understanding\nThis is revision {n} of the cancellation policy.\n{}\n한국어 끝", "unchanged context ".repeat(20));
        let unit = CurrentUnit {
            entity_id: id.clone(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    body,
                    metadata: json!({"change_id":change,"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":n as i64,"is_rule":false,"relations":[],"work_targets":[]}),
                }],
            },
        };
        versions = store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![Mutation {
                    unit,
                    expected_versions: versions,
                    evidence: format!("Observed cancellation revision {n}; preserve the reason."),
                }],
            )
            .unwrap()
            .references
            .into_iter()
            .map(|r| r.observed_version)
            .collect();
    }
    (temp, store, id, changes)
}
fn request(id: &str) -> HistoryRequest {
    HistoryRequest {
        memory_id: Some(id.into()),
        procedure_id: None,
        context_layers: Some(0),
        history_for: None,
        max_matches: None,
        start_change_id: None,
        end_change_id: None,
    }
}
#[test]
fn latest_has_compact_causal_map_not_one_node_per_revision() {
    let (temp, store, id, changes) = fixture(12);
    let value = read_history(
        &store,
        temp.path(),
        &request(&id),
        8192,
        &RequestControl::default(),
    )
    .unwrap();
    assert_eq!(value["latest_changes"].as_array().unwrap().len(), 1);
    assert_eq!(value["latest_changes"][0]["change_id"], changes[11]);
    assert_eq!(value["history_map"]["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        value["history_map"]["paths"][0]["intermediate_change_count"],
        10
    );
    assert!(value.get("response_limit").is_none());
}
#[test]
fn causal_range_is_inclusive_and_budget_provides_details_with_remaining_map() {
    let (temp, store, id, changes) = fixture(12);
    let mut r = request(&id);
    r.context_layers = None;
    r.start_change_id = Some(changes[1].clone());
    r.end_change_id = Some(changes[10].clone());
    let complete =
        read_history(&store, temp.path(), &r, 100_000, &RequestControl::default()).unwrap();
    assert_eq!(complete["changes"].as_array().unwrap().len(), 10);
    assert_eq!(complete["changes"][0]["change_id"], changes[1]);
    assert_eq!(complete["changes"][9]["change_id"], changes[10]);
    let partial = read_history(&store, temp.path(), &r, 4096, &RequestControl::default()).unwrap();
    assert!(partial["changes"].as_array().is_some(), "{partial}");
    assert!(partial["remaining_requested_changes"].as_u64().unwrap() > 0);
    assert!(serde_json::to_vec(&partial).unwrap().len() <= 4096);
    assert_eq!(
        partial["response_limit"]["config_key"],
        "history.response_limit_bytes"
    );
    r.start_change_id = Some(changes[10].clone());
    r.end_change_id = Some(changes[1].clone());
    assert_eq!(
        read_history(&store, temp.path(), &r, 8192, &RequestControl::default()).unwrap()["code"],
        "invalid_history_interval"
    );
}
#[test]
fn indivisible_change_uses_whole_frozen_file_and_search_is_ranked() {
    let (temp, store, id, _) = fixture(4);
    let mut r = request(&id);
    let small = read_history(&store, temp.path(), &r, 128, &RequestControl::default()).unwrap();
    assert_eq!(small["content_read"], false);
    let original: Value =
        serde_json::from_reader(File::open(small["artifact"]["path"].as_str().unwrap()).unwrap())
            .unwrap();
    assert!(original["latest_changes"][0]["markdown_diff"]
        .as_str()
        .unwrap()
        .contains("한국어 끝"));
    r.history_for = Some("cancellation 2".into());
    r.max_matches = Some(1);
    r.context_layers = Some(1);
    let query = read_history(&store, temp.path(), &r, 8192, &RequestControl::default()).unwrap();
    let window = query["match_windows"][0]["changes"].as_array().unwrap();
    assert_eq!(window.len(), 2);
    assert_eq!(window[1]["matched"], true);
    assert!(window[1]["evidence"]
        .as_str()
        .unwrap()
        .contains("revision 2"));
    assert!(window[0].get("matched").is_none());
}
#[test]
fn map_keeps_forks_and_all_integration_parents() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE nodes(ordinal INTEGER,id TEXT,parents INTEGER,children INTEGER,keynode INTEGER); CREATE TABLE edges(child TEXT,parent TEXT); INSERT INTO nodes VALUES(0,'a',0,2,1),(1,'b',1,1,0),(2,'c',1,1,0),(3,'d',2,0,1); INSERT INTO edges VALUES('b','a'),('c','a'),('d','b'),('d','c');").unwrap();
    let mut bytes = vec![];
    write_map(&db, &mut bytes).unwrap();
    let map: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(map["nodes"][0]["roles"], json!(["root", "fork"]));
    assert_eq!(map["nodes"][3]["roles"], json!(["head", "integration"]));
    assert_eq!(map["paths"].as_array().unwrap().len(), 4);
    // Parallel compressed paths need distinct endpoints to remain selectable:
    // fork children are anchors, otherwise b and c would become indistinguishable.
}

#[test]
fn many_branch_and_merge_objects_use_file_delivery_before_value_allocation() {
    let temp = tempfile::tempdir().unwrap();
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE nodes(ordinal INTEGER PRIMARY KEY,id TEXT UNIQUE,parents INTEGER,children INTEGER,keynode INTEGER); CREATE TABLE edges(child TEXT,parent TEXT,PRIMARY KEY(child,parent)); CREATE INDEX edge_parent ON edges(parent,child); CREATE TABLE selected(window INTEGER,id TEXT,matched INTEGER); CREATE TABLE rendered(window INTEGER,ordinal INTEGER,id TEXT,offset INTEGER,bytes INTEGER); BEGIN;").unwrap();
    let mut parent = uuid::Uuid::new_v4().to_string();
    db.execute("INSERT INTO nodes VALUES(0,?1,0,2,1)", [&parent])
        .unwrap();
    let mut ordinal = 1i64;
    for branch in 0..15_000 {
        let left = uuid::Uuid::new_v4().to_string();
        let right = uuid::Uuid::new_v4().to_string();
        let merge = uuid::Uuid::new_v4().to_string();
        for id in [&left, &right] {
            db.execute(
                "INSERT INTO nodes VALUES(?1,?2,1,1,0)",
                params![ordinal, id],
            )
            .unwrap();
            ordinal += 1;
        }
        db.execute(
            "INSERT INTO nodes VALUES(?1,?2,2,?3,1)",
            params![ordinal, merge, if branch == 14_999 { 0 } else { 2 }],
        )
        .unwrap();
        ordinal += 1;
        for (child, ancestor) in [
            (&left, &parent),
            (&right, &parent),
            (&merge, &left),
            (&merge, &right),
        ] {
            db.execute("INSERT INTO edges VALUES(?1,?2)", params![child, ancestor])
                .unwrap();
        }
        parent = merge;
    }
    db.execute_batch("COMMIT;").unwrap();
    let mut r = request(&uuid::Uuid::new_v4().to_string());
    r.history_for = Some("no matching change".into());
    let result = render_disk(
        &db,
        temp.path(),
        temp.path(),
        &r,
        128 * 1024 * 1024,
        &RequestControl::default(),
        None,
        true,
        None,
    )
    .unwrap();
    assert_eq!(result["code"], "complete_content_in_file");
    let size = result["artifact"]["bytes"].as_u64().unwrap();
    assert!(size < HISTORY_WORKSPACE_MEMORY_LIMIT);
    assert!(size > 4 * 1024 * 1024);
    assert_eq!(result["content_read"], false);
}

struct UnavailableHistory(crate::semantic::OfferResult);
struct HitHistory<'a> {
    position: agentlaw_storage::published::SourcePosition,
    hits: Vec<agentlaw_search::Hit>,
    publish: Option<(&'a Store, String, String)>,
}
impl crate::semantic::SemanticPort for HitHistory<'_> {
    fn embed_query(
        &self,
        _: &str,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::QueryVector, crate::semantic::SemanticError> {
        Ok(crate::semantic::QueryVector {
            model: "ready".into(),
            values: std::sync::Arc::from([1.0]),
        })
    }
    fn search_available(
        &self,
        _: crate::semantic::SemanticQuery<'_>,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::SemanticCandidates, crate::semantic::SemanticError>
    {
        unreachable!()
    }
}
impl crate::semantic::HistorySemanticPort for HitHistory<'_> {
    fn offer(&self, _: crate::semantic::HistoryIngestHint) -> crate::semantic::OfferResult {
        crate::semantic::OfferResult::Accepted
    }
    fn search_available(
        &self,
        _: &str,
        _: &str,
        _: &crate::semantic::QueryVector,
        _: &[String],
        _: usize,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::SemanticCandidates, crate::semantic::SemanticError>
    {
        let mut position = self.position.clone();
        if let Some((store, subject, next)) = &self.publish {
            let current = store.read_current(subject).unwrap();
            let head = match &current.state {
                UnitState::Live { heads } => &heads[0],
                _ => unreachable!(),
            };
            let expected = agentlaw_storage::version(&current, head).unwrap();
            let mut metadata = head.metadata.clone();
            metadata["change_id"] = json!(next);
            let unit = CurrentUnit {
                entity_id: subject.clone(),
                entity_type: "memory".into(),
                state: UnitState::Live {
                    heads: vec![Head {
                        body: "newly published revision".into(),
                        metadata,
                    }],
                },
            };
            let receipt = store
                .publish(
                    &uuid::Uuid::new_v4().to_string(),
                    vec![Mutation {
                        unit,
                        expected_versions: vec![expected],
                        evidence: "new change".into(),
                    }],
                )
                .unwrap();
            position.sequence = receipt.generation;
        }
        Ok(crate::semantic::SemanticCandidates {
            hits: self.hits.clone(),
            coverage: crate::semantic::SemanticCoverage::CompleteAt(position),
        })
    }
}
struct ReadyHistory {
    offer: crate::semantic::OfferResult,
    position: agentlaw_storage::published::SourcePosition,
}
impl crate::semantic::SemanticPort for ReadyHistory {
    fn embed_prefix(
        &self,
        text: &str,
        control: &RequestControl,
    ) -> std::result::Result<crate::semantic::PrefixEmbedding, crate::semantic::SemanticError> {
        Ok(crate::semantic::PrefixEmbedding {
            vector: self.embed_query(text, control)?,
            truncated: text.split_whitespace().count() > 512,
        })
    }
    fn embed_query(
        &self,
        _: &str,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::QueryVector, crate::semantic::SemanticError> {
        Ok(crate::semantic::QueryVector {
            model: "ready".into(),
            values: std::sync::Arc::from([1.0]),
        })
    }
    fn search_available(
        &self,
        _: crate::semantic::SemanticQuery<'_>,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::SemanticCandidates, crate::semantic::SemanticError>
    {
        unreachable!()
    }
}
impl crate::semantic::HistorySemanticPort for ReadyHistory {
    fn offer(&self, _: crate::semantic::HistoryIngestHint) -> crate::semantic::OfferResult {
        self.offer
    }
    fn search_available(
        &self,
        _: &str,
        _: &str,
        _: &crate::semantic::QueryVector,
        _: &[String],
        _: usize,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::SemanticCandidates, crate::semantic::SemanticError>
    {
        Ok(crate::semantic::SemanticCandidates {
            hits: vec![],
            coverage: crate::semantic::SemanticCoverage::CompleteAt(self.position.clone()),
        })
    }
}
impl crate::semantic::SemanticPort for UnavailableHistory {
    fn embed_query(
        &self,
        _: &str,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::QueryVector, crate::semantic::SemanticError> {
        Err(crate::semantic::SemanticError::Unavailable(
            "not ready".into(),
        ))
    }
    fn search_available(
        &self,
        _: crate::semantic::SemanticQuery<'_>,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::SemanticCandidates, crate::semantic::SemanticError>
    {
        Err(crate::semantic::SemanticError::Unavailable(
            "not ready".into(),
        ))
    }
}
impl crate::semantic::HistorySemanticPort for UnavailableHistory {
    fn offer(&self, _: crate::semantic::HistoryIngestHint) -> crate::semantic::OfferResult {
        self.0
    }
    fn search_available(
        &self,
        _: &str,
        _: &str,
        _: &crate::semantic::QueryVector,
        _: &[String],
        _: usize,
        _: &RequestControl,
    ) -> std::result::Result<crate::semantic::SemanticCandidates, crate::semantic::SemanticError>
    {
        Err(crate::semantic::SemanticError::Unavailable(
            "not ready".into(),
        ))
    }
}

#[test]
fn semantic_candidates_use_the_acquired_change_id_and_epoch() {
    let (temp, store, id, changes) = fixture(2);
    let control_dir = temp.path().join("control");
    std::fs::create_dir(&control_dir).unwrap();
    let _writer = crate::control::ControlWriter::open(&control_dir).unwrap();
    let reader = crate::control::ControlReader::open(&control_dir).unwrap();
    let source = store.owned_published_reader();
    let position = source.read_snapshot().unwrap().position().clone();
    let cache = crate::history_cache::HistoryCacheGuards::new();
    let control = RequestControl::default();
    let identity = crate::store::StoreIdentity {
        root: temp.path().join("source"),
        local: temp.path().into(),
        user_id: "user".into(),
        machine_id: "machine".into(),
        binding_id: "binding".into(),
        history_limit: 65536,
        response_limit: 65536,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    let mut ordered = changes.clone();
    ordered.sort();
    let hit = |id: &str| agentlaw_search::Hit {
        memory_id: format!("change:{id}"),
        change_id: id.into(),
        score: 0.9,
    };
    let semantic = HitHistory {
        position: position.clone(),
        hits: vec![hit(&ordered[1])],
        publish: None,
    };
    let ctx = crate::store::ReadCtx {
        identity: &identity,
        source: &source,
        control: &reader,
        semantic: &semantic,
        history_semantic: &semantic,
        history_cache: &cache,
        request: &control,
    };
    let mut query = request(&id);
    query.history_for = Some("cancellation".into());
    query.max_matches = Some(2);
    let result = history(&ctx, &query).unwrap();
    let windows = result["match_windows"].as_array().unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0]["changes"][0]["change_id"], ordered[1]);
    assert_eq!(windows[1]["changes"][0]["change_id"], ordered[0]);

    let mut foreign = position.clone();
    foreign.epoch = uuid::Uuid::new_v4().to_string();
    let semantic = HitHistory {
        position: foreign,
        hits: vec![hit(&ordered[1])],
        publish: None,
    };
    let ctx = crate::store::ReadCtx {
        identity: &identity,
        source: &source,
        control: &reader,
        semantic: &semantic,
        history_semantic: &semantic,
        history_cache: &cache,
        request: &control,
    };
    query.max_matches = Some(1);
    let result = history(&ctx, &query).unwrap();
    assert_eq!(
        result["match_windows"][0]["changes"][0]["change_id"],
        ordered[0]
    );
    assert_eq!(
        result["diagnostics"][0]["code"],
        "semantic_channel_incomplete"
    );

    let next = uuid::Uuid::new_v4().to_string();
    let semantic = HitHistory {
        position,
        hits: vec![hit(&next)],
        publish: Some((&store, id.clone(), next.clone())),
    };
    let ctx = crate::store::ReadCtx {
        identity: &identity,
        source: &source,
        control: &reader,
        semantic: &semantic,
        history_semantic: &semantic,
        history_cache: &cache,
        request: &control,
    };
    let result = history(&ctx, &query).unwrap();
    assert_eq!(result["match_windows"].as_array().unwrap().len(), 1);
    assert_eq!(
        result["match_windows"][0]["changes"][0]["change_id"],
        ordered[0]
    );
    assert!(store
        .read_current(&id)
        .unwrap()
        .heads()
        .iter()
        .any(|head| head.metadata["change_id"] == next));
}

#[test]
fn owned_history_uses_memory_and_reports_unavailable_semantics() {
    use std::collections::BTreeMap;
    fn files(root: &Path, result: &mut BTreeMap<std::path::PathBuf, (u64, std::time::SystemTime)>) {
        for item in std::fs::read_dir(root).unwrap() {
            let item = item.unwrap();
            let metadata = item.metadata().unwrap();
            if metadata.is_dir() {
                files(&item.path(), result);
            } else {
                result.insert(item.path(), (metadata.len(), metadata.modified().unwrap()));
            }
        }
    }
    let (temp, store, id, changes) = fixture(3);
    let control_dir = temp.path().join("control");
    std::fs::create_dir(&control_dir).unwrap();
    let _writer = crate::control::ControlWriter::open(&control_dir).unwrap();
    let reader = crate::control::ControlReader::open(&control_dir).unwrap();
    let source = store.owned_published_reader();
    let cache = crate::history_cache::HistoryCacheGuards::new();
    let control = RequestControl::default();
    let identity = crate::store::StoreIdentity {
        root: temp.path().join("source"),
        local: temp.path().into(),
        user_id: "user".into(),
        machine_id: "machine".into(),
        binding_id: "binding".into(),
        history_limit: 65536,
        response_limit: 65536,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    for offer in [
        crate::semantic::OfferResult::Busy,
        crate::semantic::OfferResult::Queued,
        crate::semantic::OfferResult::Accepted,
    ] {
        let ports = UnavailableHistory(offer);
        let ctx = crate::store::ReadCtx {
            identity: &identity,
            source: &source,
            control: &reader,
            semantic: &ports,
            history_semantic: &ports,
            history_cache: &cache,
            request: &control,
        };
        let mut before = BTreeMap::new();
        files(temp.path(), &mut before);
        let mut r = request(&id);
        r.history_for = Some("cancellation revision".into());
        let value = history(&ctx, &r).unwrap();
        assert!(!value["match_windows"].as_array().unwrap().is_empty());
        assert!(value["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["code"] == "semantic_channel_incomplete"));
        let mut after = BTreeMap::new();
        files(temp.path(), &mut after);
        assert_eq!(before, after);
        let latest = history(&ctx, &request(&id)).unwrap();
        assert_eq!(latest["latest_changes"][0]["change_id"], changes[2]);
    }
    let position = source.read_snapshot().unwrap().position().clone();
    for offer in [
        crate::semantic::OfferResult::Busy,
        crate::semantic::OfferResult::Queued,
    ] {
        let ports = ReadyHistory {
            offer,
            position: position.clone(),
        };
        let ctx = crate::store::ReadCtx {
            identity: &identity,
            source: &source,
            control: &reader,
            semantic: &ports,
            history_semantic: &ports,
            history_cache: &cache,
            request: &control,
        };
        let mut r = request(&id);
        r.history_for = Some("cancellation".into());
        let result = history(&ctx, &r).unwrap();
        assert_eq!(
            result["diagnostics"][0]["code"],
            "semantic_channel_incomplete"
        );
    }
    let ready = ReadyHistory {
        offer: crate::semantic::OfferResult::Accepted,
        position: position.clone(),
    };
    let ready_ctx = crate::store::ReadCtx {
        identity: &identity,
        source: &source,
        control: &reader,
        semantic: &ready,
        history_semantic: &ready,
        history_cache: &cache,
        request: &control,
    };
    let latest_request = request(&id);
    assert_eq!(
        history(&ready_ctx, &latest_request).unwrap(),
        read_history(
            &store,
            temp.path(),
            &latest_request,
            identity.history_limit,
            &control
        )
        .unwrap()
    );
    let mut range = request(&id);
    range.start_change_id = Some(changes[0].clone());
    range.end_change_id = Some(changes[2].clone());
    assert_eq!(
        history(&ready_ctx, &range).unwrap(),
        read_history(
            &store,
            temp.path(),
            &range,
            identity.history_limit,
            &control
        )
        .unwrap()
    );
    let mut boundary_request = request(&id);
    boundary_request.history_for = Some("cancellation".into());
    let boundary = serde_json::to_vec(&history(&ready_ctx, &boundary_request).unwrap())
        .unwrap()
        .len();
    let boundary_identity = crate::store::StoreIdentity {
        history_limit: boundary,
        root: identity.root.clone(),
        local: identity.local.clone(),
        user_id: identity.user_id.clone(),
        machine_id: identity.machine_id.clone(),
        binding_id: identity.binding_id.clone(),
        response_limit: identity.response_limit,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    let busy = ReadyHistory {
        offer: crate::semantic::OfferResult::Busy,
        position,
    };
    let busy_ctx = crate::store::ReadCtx {
        identity: &boundary_identity,
        source: &source,
        control: &reader,
        semantic: &busy,
        history_semantic: &busy,
        history_cache: &cache,
        request: &control,
    };
    let result = history(&busy_ctx, &boundary_request).unwrap();
    if result["code"] == "complete_content_in_file" {
        let content: Value = serde_json::from_reader(
            File::open(result["artifact"]["path"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            content["diagnostics"][0]["code"],
            "semantic_channel_incomplete"
        );
    } else {
        assert!(serde_json::to_vec(&result).unwrap().len() <= boundary);
        assert_eq!(
            result["diagnostics"][0]["code"],
            "semantic_channel_incomplete"
        );
    }
    let ports = UnavailableHistory(crate::semantic::OfferResult::Accepted);
    let small = crate::store::StoreIdentity {
        history_limit: 128,
        ..identity
    };
    let ctx = crate::store::ReadCtx {
        identity: &small,
        source: &source,
        control: &reader,
        semantic: &ports,
        history_semantic: &ports,
        history_cache: &cache,
        request: &control,
    };
    let result = history(&ctx, &request(&id)).unwrap();
    assert_eq!(result["content_read"], false);
    let artifact: Value =
        serde_json::from_reader(File::open(result["artifact"]["path"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(artifact["latest_changes"][0]["change_id"], changes[2]);
}

#[test]
fn projected_work_that_can_exceed_budget_uses_a_temporary_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let local = temp.path().join("binding");
    let store = Store::open(temp.path().join("source"), local.join("canonical")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                body: "term ".repeat(60_000),
                metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
            }],
        },
    };
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "observation".into(),
            }],
        )
        .unwrap();
    let source = store.owned_published_reader();
    let mut spool = source
        .acquire_history(&std::sync::atomic::AtomicBool::new(false))
        .unwrap()
        .into_spool()
        .unwrap();
    assert!(spool.size_bytes().unwrap() < HISTORY_WORKSPACE_MEMORY_LIMIT);
    let mut r = request(&id);
    r.history_for = Some("term".into());
    let workspace =
        ProjectionWorkspace::for_spool(&local, &mut spool, &id, &r, &RequestControl::default())
            .unwrap();
    let directory = workspace.directory.as_ref().unwrap().path().to_path_buf();
    assert!(directory.join("projection.sqlite").is_file());
    assert_eq!(
        workspace
            .db
            .query_row("PRAGMA temp_store", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        workspace
            .db
            .query_row("PRAGMA cache_size", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        -2048
    );
    assert_eq!(
        workspace
            .db
            .query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(workspace);
    assert!(!directory.exists());
}

#[test]
fn memory_and_file_workspace_artifacts_share_guidance_and_complete_content() {
    let mut previous_guidance = None;
    for size in [100, 300_000] {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("binding");
        let store = Store::open(temp.path().join("source"), local.join("canonical")).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let unit = CurrentUnit {
            entity_id: id.clone(),
            entity_type: "memory".into(),
            state: UnitState::Live {
                heads: vec![Head {
                    body: "x".repeat(size),
                    metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                }],
            },
        };
        store
            .publish(
                &uuid::Uuid::new_v4().to_string(),
                vec![Mutation {
                    unit,
                    expected_versions: vec![],
                    evidence: "observation".into(),
                }],
            )
            .unwrap();
        let control_dir = temp.path().join("control");
        std::fs::create_dir(&control_dir).unwrap();
        let _writer = crate::control::ControlWriter::open(&control_dir).unwrap();
        let control_reader = crate::control::ControlReader::open(&control_dir).unwrap();
        let source = store.owned_published_reader();
        let cache = crate::history_cache::HistoryCacheGuards::new();
        let control = RequestControl::default();
        let identity = crate::store::StoreIdentity {
            root: temp.path().join("source"),
            local: local.clone(),
            user_id: "user".into(),
            machine_id: "machine".into(),
            binding_id: "binding".into(),
            history_limit: 128,
            response_limit: 128,
            config_file: Some(std::path::PathBuf::from("test-config.json")),
        };
        let ports = UnavailableHistory(crate::semantic::OfferResult::Accepted);
        let ctx = crate::store::ReadCtx {
            identity: &identity,
            source: &source,
            control: &control_reader,
            semantic: &ports,
            history_semantic: &ports,
            history_cache: &cache,
            request: &control,
        };
        let expected = read_history(&store, &local, &request(&id), 2_000_000, &control).unwrap();
        let result = history(&ctx, &request(&id)).unwrap();
        assert_eq!(result["code"], "complete_content_in_file");
        let mut actual: Value = serde_json::from_reader(
            File::open(result["artifact"]["path"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        // The file body carries the limit notice with the installation config
        // path; everything else is the complete history result.
        assert_eq!(
            actual["response_limit"]["config_file"],
            json!(identity.config_file)
        );
        actual.as_object_mut().unwrap().remove("response_limit");
        assert_eq!(actual, expected);
        let guidance = result["next_action"].as_str().unwrap();
        assert!(guidance.contains("cannot access the path"));
        assert!(guidance.contains("retention period"));
        assert!(guidance.contains("then-current source"));
        if let Some(previous) = previous_guidance {
            assert_eq!(previous, guidance);
        }
        previous_guidance = Some(guidance.to_owned());
    }
}

#[test]
fn oversized_acquired_history_uses_a_removed_temporary_workspace() {
    let _large = LARGE_HISTORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let local = temp.path().join("binding");
    let store = Store::open(temp.path().join("source"), local.join("canonical")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                body: "x".repeat(65 * 1024 * 1024),
                metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
            }],
        },
    };
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "oversized history".into(),
            }],
        )
        .unwrap();
    let reader = store.owned_published_reader();
    let acquired = reader
        .acquire_history(&std::sync::atomic::AtomicBool::new(false))
        .unwrap();
    let mut spool = acquired.into_spool().unwrap();
    assert!(spool.size_bytes().unwrap() > HISTORY_WORKSPACE_MEMORY_LIMIT);
    let workspace = ProjectionWorkspace::for_spool(
        &local,
        &mut spool,
        &id,
        &request(&id),
        &RequestControl::default(),
    )
    .unwrap();
    let path = workspace.directory.as_ref().unwrap().path().to_path_buf();
    assert!(path.join("projection.sqlite").is_file());
    assert_eq!(
        workspace
            .db
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "off"
    );
    drop(workspace);
    assert!(!path.exists());
    drop(spool);
    let control_dir = temp.path().join("control");
    std::fs::create_dir(&control_dir).unwrap();
    let _writer = crate::control::ControlWriter::open(&control_dir).unwrap();
    let control_reader = crate::control::ControlReader::open(&control_dir).unwrap();
    let cache = crate::history_cache::HistoryCacheGuards::new();
    let control = RequestControl::default();
    let identity = crate::store::StoreIdentity {
        root: temp.path().join("source"),
        local: local.clone(),
        user_id: "user".into(),
        machine_id: "machine".into(),
        binding_id: "binding".into(),
        history_limit: 128 * 1024 * 1024,
        response_limit: 128 * 1024 * 1024,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    let ports = UnavailableHistory(crate::semantic::OfferResult::Accepted);
    let ctx = crate::store::ReadCtx {
        identity: &identity,
        source: &reader,
        control: &control_reader,
        semantic: &ports,
        history_semantic: &ports,
        history_cache: &cache,
        request: &control,
    };
    let result = history(&ctx, &request(&id)).unwrap();
    assert_eq!(result["code"], "complete_content_in_file");
    assert_eq!(result["content_read"], false);
    assert!(result["artifact"]["bytes"].as_u64().unwrap() > HISTORY_WORKSPACE_MEMORY_LIMIT);
    assert_eq!(
        result["response_limit"]["in_memory_budget_bytes"],
        HISTORY_WORKSPACE_MEMORY_LIMIT
    );
    let mut prefix = [0u8; 256];
    let mut input = File::open(result["artifact"]["path"].as_str().unwrap()).unwrap();
    let n = input.read(&mut prefix).unwrap();
    assert!(std::str::from_utf8(&prefix[..n])
        .unwrap()
        .contains("latest_changes"));
}

#[test]
fn serialized_response_below_budget_is_file_delivered_when_assembly_would_exceed_it() {
    let _large = LARGE_HISTORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let local = temp.path().join("binding");
    let store = Store::open(temp.path().join("source"), local.join("canonical")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let unit = CurrentUnit {
        entity_id: id.clone(),
        entity_type: "memory".into(),
        state: UnitState::Live {
            heads: vec![Head {
                body: "x".repeat(40 * 1024 * 1024),
                metadata: json!({"change_id":uuid::Uuid::new_v4().to_string(),"applicability":{"scope":"user"},"origin":{"machine_id":uuid::Uuid::new_v4().to_string()},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
            }],
        },
    };
    store
        .publish(
            &uuid::Uuid::new_v4().to_string(),
            vec![Mutation {
                unit,
                expected_versions: vec![],
                evidence: "history".into(),
            }],
        )
        .unwrap();
    let control_dir = temp.path().join("control");
    std::fs::create_dir(&control_dir).unwrap();
    let _writer = crate::control::ControlWriter::open(&control_dir).unwrap();
    let control_reader = crate::control::ControlReader::open(&control_dir).unwrap();
    let source = store.owned_published_reader();
    let cache = crate::history_cache::HistoryCacheGuards::new();
    let control = RequestControl::default();
    let identity = crate::store::StoreIdentity {
        root: temp.path().join("source"),
        local,
        user_id: "user".into(),
        machine_id: "machine".into(),
        binding_id: "binding".into(),
        history_limit: 128 * 1024 * 1024,
        response_limit: 128 * 1024 * 1024,
        config_file: Some(std::path::PathBuf::from("test-config.json")),
    };
    let ports = UnavailableHistory(crate::semantic::OfferResult::Accepted);
    let ctx = crate::store::ReadCtx {
        identity: &identity,
        source: &source,
        control: &control_reader,
        semantic: &ports,
        history_semantic: &ports,
        history_cache: &cache,
        request: &control,
    };
    let result = history(&ctx, &request(&id)).unwrap();
    assert_eq!(result["code"], "complete_content_in_file");
    assert_eq!(result["content_read"], false);
    let bytes = result["artifact"]["bytes"].as_u64().unwrap();
    assert!(bytes > 40 * 1024 * 1024 && bytes < HISTORY_WORKSPACE_MEMORY_LIMIT);
    assert!(bytes < identity.history_limit as u64);
    assert!(!inline_assembly_fits(
        &mut File::open(result["artifact"]["path"].as_str().unwrap()).unwrap(),
        &control
    )
    .unwrap());
    assert_eq!(
        result["response_limit"]["in_memory_budget_bytes"],
        HISTORY_WORKSPACE_MEMORY_LIMIT
    );
    let mut prefix = [0u8; 256];
    let mut input = File::open(result["artifact"]["path"].as_str().unwrap()).unwrap();
    let n = input.read(&mut prefix).unwrap();
    assert!(std::str::from_utf8(&prefix[..n])
        .unwrap()
        .contains("latest_changes"));
}

fn read_history(
    store: &Store,
    local: &Path,
    request: &HistoryRequest,
    limit: usize,
    control: &RequestControl,
) -> crate::Result<Value> {
    let identity = crate::store::StoreIdentity {
        root: local.join("source"),
        local: local.to_path_buf(),
        user_id: "user".into(),
        machine_id: uuid::Uuid::new_v4().to_string(),
        binding_id: uuid::Uuid::new_v4().to_string(),
        history_limit: limit,
        response_limit: usize::MAX,
        config_file: None,
    };
    let _writer = crate::control::ControlWriter::open(local)?;
    let reader = crate::control::ControlReader::open(local)?;
    let source = store.owned_published_reader();
    let cache = crate::history_cache::HistoryCacheGuards::new();
    let ports = UnavailableHistory(crate::semantic::OfferResult::Accepted);
    history(
        &crate::store::ReadCtx {
            identity: &identity,
            source: &source,
            control: &reader,
            semantic: &ports,
            history_semantic: &ports,
            history_cache: &cache,
            request: control,
        },
        request,
    )
}

fn long_history_query_diagnostics(ready: bool, partial: bool) {
    use crate::semantic::{
        HistoryIngestHint, HistorySemanticPort, OfferResult, PrefixEmbedding, QueryVector,
        SemanticCandidates, SemanticCoverage, SemanticError, SemanticPort, SemanticQuery,
    };
    struct Model {
        ready: bool,
        coverage: SemanticCoverage,
    }
    impl SemanticPort for Model {
        fn embed_query(
            &self,
            _: &str,
            _: &RequestControl,
        ) -> std::result::Result<QueryVector, SemanticError> {
            if !self.ready {
                return Err(SemanticError::Unavailable("model loading".into()));
            }
            Ok(QueryVector {
                model: "model".into(),
                values: std::sync::Arc::from([1.0]),
            })
        }
        fn embed_prefix(
            &self,
            text: &str,
            control: &RequestControl,
        ) -> std::result::Result<PrefixEmbedding, SemanticError> {
            assert!(text.ends_with("cancellation"));
            Ok(PrefixEmbedding {
                vector: self.embed_query(text, control)?,
                truncated: true,
            })
        }
        fn search_available(
            &self,
            _: SemanticQuery<'_>,
            _: &RequestControl,
        ) -> std::result::Result<SemanticCandidates, SemanticError> {
            unreachable!()
        }
    }
    impl HistorySemanticPort for Model {
        fn offer(&self, _: HistoryIngestHint) -> OfferResult {
            OfferResult::Accepted
        }
        fn search_available(
            &self,
            _: &str,
            _: &str,
            _: &QueryVector,
            _: &[String],
            _: usize,
            _: &RequestControl,
        ) -> std::result::Result<SemanticCandidates, SemanticError> {
            Ok(SemanticCandidates {
                hits: vec![],
                coverage: self.coverage.clone(),
            })
        }
    }
    let (temp, store, id, _) = fixture(3);
    let control_dir = temp.path().join("control");
    std::fs::create_dir(&control_dir).unwrap();
    let _writer = crate::control::ControlWriter::open(&control_dir).unwrap();
    let reader = crate::control::ControlReader::open(&control_dir).unwrap();
    let source = store.owned_published_reader();
    let cache = crate::history_cache::HistoryCacheGuards::new();
    let control = RequestControl::default();
    let identity = crate::store::StoreIdentity {
        root: temp.path().join("source"),
        local: temp.path().into(),
        user_id: "user".into(),
        machine_id: "machine".into(),
        binding_id: "binding".into(),
        history_limit: 65536,
        response_limit: 65536,
        config_file: None,
    };
    let requested = source.read_snapshot().unwrap().position().clone();
    let coverage = if partial {
        let mut available = requested.clone();
        available.sequence -= 1;
        SemanticCoverage::Partial {
            available,
            requested,
        }
    } else {
        SemanticCoverage::CompleteAt(requested)
    };
    let model = Model { ready, coverage };
    let ctx = crate::store::ReadCtx {
        identity: &identity,
        source: &source,
        control: &reader,
        semantic: &model,
        history_semantic: &model,
        history_cache: &cache,
        request: &control,
    };
    let mut query = request(&id);
    query.history_for = Some(format!("{}cancellation", "unmatchedword ".repeat(599)));

    let response = history(&ctx, &query).unwrap();
    assert!(!response["match_windows"].as_array().unwrap().is_empty());
    let codes: Vec<_> = response["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|diagnostic| diagnostic["code"].as_str())
        .collect();
    assert_eq!(
        codes
            .iter()
            .filter(|&&code| code == "semantic_query_truncated")
            .count(),
        usize::from(ready),
    );
    assert_eq!(
        codes.contains(&"semantic_channel_incomplete"),
        partial || !ready
    );
}

#[test]
fn long_history_query_reports_truncation_with_complete_coverage() {
    long_history_query_diagnostics(true, false);
}
#[test]
fn long_history_query_reports_truncation_with_partial_coverage() {
    long_history_query_diagnostics(true, true);
}
#[test]
fn long_history_query_loading_model_reports_only_incompleteness() {
    long_history_query_diagnostics(false, false);
}
