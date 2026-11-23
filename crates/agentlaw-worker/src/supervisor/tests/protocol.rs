use super::*;

#[test]
fn model_request_ids_are_shared_and_checked() {
    let ids = ModelRequestIds::new();
    let other = ids.clone();
    assert_eq!(ids.next(), Some(ModelRequestId(1)));
    assert_eq!(other.next(), Some(ModelRequestId(2)));
    *ids.counter.lock().unwrap() = u64::MAX - 1;
    assert_eq!(other.next(), Some(ModelRequestId(u64::MAX)));
    assert_eq!(ids.next(), None);
    assert_eq!(other.next(), None);
}

#[test]
fn model_protocol_preserves_full_unsigned_request_id() {
    let request = Input::Embed {
        overflow: crate::model_child::Overflow::Reject,
        id: u64::MAX,
        attempt: 1,
        text: "query".into(),
    };
    let frame = model_child::encode(&request).unwrap();
    let decoded: Input = model_child::read(&mut frame.as_slice()).unwrap();
    assert!(matches!(decoded, Input::Embed { id: u64::MAX, .. }));
}
