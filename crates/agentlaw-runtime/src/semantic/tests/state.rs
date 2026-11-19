use super::super::source::Piece;
use super::super::state::{Backoff, QueuedPiece, Schedule};
use super::*;

fn piece(text: &str) -> Piece {
    Piece {
        memory_id: text.into(),
        change_id: "change".into(),
        section: "body:0".into(),
        scope: "user".into(),
        text: text.into(),
    }
}

#[test]
fn backoff_doubles_and_caps_then_resets() {
    let now = Instant::now();
    let mut retry = Backoff::default();
    for seconds in [1, 2, 4, 8, 16, 30, 30] {
        retry.fail(now);
        assert_eq!(retry.until, Some(now + Duration::from_secs(seconds)));
    }
    retry.reset();
    retry.fail(now);
    assert_eq!(retry.until, Some(now + Duration::from_secs(1)));
}

#[test]
fn round_robin_respects_capacity_and_digest() {
    let mut schedule = Schedule {
        model: Some("model".into()),
        ..Default::default()
    };
    for name in ["a", "b"] {
        let mut lane = Lane::default();
        lane.queue.push_back(QueuedPiece::new(piece(name), false));
        lane.queue.push_back(QueuedPiece::new(piece(name), false));
        schedule.lanes.insert(LaneKey::current(name), lane);
    }
    let ids = ModelRequestIds::new();
    let now = Instant::now();
    assert!(schedule.next_task("other", &ids, 1, now).is_none());
    assert_eq!(
        indexed_text(&schedule.next_task("model", &ids, 1, now).unwrap()),
        "a"
    );
    assert_eq!(
        indexed_text(&schedule.next_task("model", &ids, 1, now).unwrap()),
        "b"
    );
    assert!(schedule.next_task("model", &ids, 1, now).is_none());
    schedule
        .lanes
        .get_mut(&LaneKey::current("a"))
        .unwrap()
        .invalidate();
    assert_eq!(schedule.lanes[&LaneKey::current("a")].occupied(), 0);
}

#[test]
fn round_robin_alternates_while_both_bindings_have_capacity() {
    let mut schedule = Schedule {
        model: Some("model".into()),
        ..Default::default()
    };
    for name in ["a", "b"] {
        let mut lane = Lane::default();
        for _ in 0..3 {
            lane.queue.push_back(QueuedPiece::new(piece(name), false));
        }
        schedule.lanes.insert(LaneKey::current(name), lane);
    }
    let ids = ModelRequestIds::new();
    let now = Instant::now();
    for expected in ["a", "b", "a", "b"] {
        assert_eq!(
            indexed_text(&schedule.next_task("model", &ids, 3, now).unwrap()),
            expected
        );
    }
}
