use super::*;
use crate::DIMENSIONS;

#[test]
fn queries_share_one_computation_and_cancel_independently() {
    let ids = ModelRequestIds::new();
    let mut queue = QueryQueue::default();
    let first = queue.submit("model", "same", &ids).unwrap();
    assert_eq!(queue.submit("model", "same", &ids).unwrap(), first);
    assert_eq!(queue.take(), Some(first));
    queue.release(first);
    assert_eq!(queue.flights[&first].waiters, 1);
    queue.release(first);
    assert!(
        queue.flights.contains_key(&first),
        "running work owns its slot until it completes"
    );
    queue.complete(first, Err(QueryError::Failed("exit".into())));
    assert!(!queue.flights.contains_key(&first));
}

#[test]
fn completed_query_is_not_overwritten_by_later_child_exit() {
    let ids = ModelRequestIds::new();
    let mut queue = QueryQueue::default();
    let id = queue.submit("model", "text", &ids).unwrap();
    queue.take();
    let mut vector = vec![0.0; DIMENSIONS];
    vector[0] = 1.0;
    let result = validate_query_vector("model".into(), vector).unwrap();
    queue.complete(id, Ok(result.clone()));
    queue.fail_all("child exited");
    assert_eq!(queue.flights[&id].result, Some(Ok(result)));
}

#[test]
fn query_capacity_and_vector_validation_are_explicit() {
    let ids = ModelRequestIds::new();
    let mut queue = QueryQueue::default();
    for i in 0..64 {
        queue.submit("model", &format!("{i}"), &ids).unwrap();
    }
    assert_eq!(
        queue.submit("model", "overflow", &ids),
        Err(QueryError::Busy)
    );
    assert_eq!(queue.submit("model", "1", &ids), Ok(ModelRequestId(2)));
    assert!(validate_query_vector("model".into(), vec![0.0; DIMENSIONS]).is_err());
    assert!(validate_query_vector("model".into(), vec![f32::NAN; DIMENSIONS]).is_err());
    assert!(validate_query_vector("model".into(), vec![1.0]).is_err());
    let mut vector = vec![0.0; DIMENSIONS];
    vector[0] = 1.0;
    assert_eq!(
        validate_query_vector("actual".into(), vector)
            .unwrap()
            .model_digest,
        "actual"
    );
}

#[test]
fn running_prefix_shares_original_query_but_not_split_or_a_different_suffix() {
    let ids = ModelRequestIds::new();
    let mut queue = QueryQueue::default();
    let transported = "a".repeat(4 * 1024 * 1024);
    let original = format!("{transported} first");
    let different = format!("{transported} second");
    let prefix = queue
        .submit_operation(
            QueryOperation::Prefix,
            "model",
            &original,
            &transported,
            &ids,
        )
        .unwrap();
    assert_eq!(queue.take(), Some(prefix));
    // Leave the first work running until both arrivals have registered.
    assert_eq!(
        queue.submit_operation(
            QueryOperation::Prefix,
            "model",
            &original,
            &transported,
            &ids
        ),
        Ok(prefix)
    );
    let second = queue
        .submit_operation(
            QueryOperation::Prefix,
            "model",
            &different,
            &transported,
            &ids,
        )
        .unwrap();
    assert_ne!(prefix, second);
    let split = queue
        .submit_operation(
            QueryOperation::Split,
            "model",
            "same window",
            "same window",
            &ids,
        )
        .unwrap();
    let window_prefix = queue
        .submit_operation(
            QueryOperation::Prefix,
            "model",
            "same window",
            "same window",
            &ids,
        )
        .unwrap();
    assert_ne!(split, window_prefix);
    assert_eq!(queue.take(), Some(second));
    assert_eq!(queue.take(), Some(split));
    assert_eq!(queue.take(), Some(window_prefix));
    assert_eq!(queue.take(), None);
}
