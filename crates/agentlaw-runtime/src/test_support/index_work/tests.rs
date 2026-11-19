use super::*;

#[test]
fn only_current_digest_receives_queued_work() {
    let fake = FakeIndexWork::new();
    let id = fake.push("queued");
    fake.model_ready("A");
    fake.model_ready("B");
    assert!(fake.next_work("A").is_none());
    assert_eq!(fake.next_work("B").unwrap().id, id);
    assert_eq!(fake.ready_digests(), vec!["A", "B"]);
}

#[test]
fn bindings_share_request_ids() {
    let ids = FakeRequestIds::new();
    let a = FakeIndexWork::with_ids(ids.clone());
    let b = FakeIndexWork::with_ids(ids);
    let issued = [a.push("a"), b.push("b"), a.push("c"), b.push("d")];
    for (i, id) in issued.iter().enumerate() {
        assert!(!issued[..i].contains(id));
    }
    assert_eq!(issued[0], ModelRequestId(1));
}

#[test]
fn work_is_fifo_and_requires_ready_digest() {
    let fake = FakeIndexWork::new();
    let a = fake.push("a");
    let b = fake.push("b");
    assert!(fake.status().runnable);
    assert!(fake.next_work("model").is_none());
    fake.set_awaiting_model(true);
    fake.model_ready("model");
    assert!(!fake.status().awaiting_model);
    assert!(fake.next_work("other").is_none());
    assert_eq!(fake.next_work("model").unwrap().id, a);
    fake.finish(
        a,
        Ok(agentlaw_worker::supervisor::IndexResult::Vector(vec![1.0])),
    );
    assert_eq!(fake.next_work("model").unwrap().id, b);
    assert!(!fake.status().runnable);
    assert_ne!(fake.push("c"), a);
    assert_eq!(
        fake.finished(),
        vec![(a, Ok(IndexResult::Vector(vec![1.0])))]
    );
}
