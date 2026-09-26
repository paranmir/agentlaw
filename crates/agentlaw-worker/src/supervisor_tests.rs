use super::*;
use crate::{Admission, JobKey, DIMENSIONS};

fn ready(b: &mut Broker) -> String {
    let inc = b
        .reserve_worker_start("model-a", now())
        .unwrap()
        .incarnation;
    b.transition(&inc, WorkerState::Loading, now()).unwrap();
    b.transition(&inc, WorkerState::Ready, now()).unwrap();
    inc
}
fn enqueue(b: &mut Broker, name: &str, durable: bool, lease: Option<&str>) -> i64 {
    let result = b
        .enqueue(
            &JobKey {
                model_digest: "model-a".into(),
                config_digest: "config".into(),
                memory_id: name.into(),
                change_id: "v1".into(),
                content_digest: name.into(),
                section: "body".into(),
            },
            name,
            durable,
            lease.is_some(),
            lease,
        )
        .unwrap();
    match result {
        Admission::Accepted(id) | Admission::Joined(id) => id,
    }
}
fn state(b: &Broker, id: i64) -> String {
    b.connection
        .query_row("SELECT state FROM jobs WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

#[test]
fn lost_worker_retries_only_its_unfinished_claim_and_preserves_results_and_ack() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broker.sqlite");
    let mut b = Broker::open(&path, 64).unwrap();
    initialize(&b).unwrap();
    let inc = ready(&mut b);
    let mut vector = vec![0.; DIMENSIONS];
    vector[0] = 1.;
    let ack = enqueue(&mut b, "already indexed", true, None);
    let job = b.claim(&inc, now()).unwrap().unwrap();
    b.complete(&job, &vector).unwrap();
    b.acknowledge(ack).unwrap();
    let completed = enqueue(&mut b, "finished inference", true, None);
    let job = b.claim(&inc, now()).unwrap().unwrap();
    b.complete(&job, &vector).unwrap();
    let permanent = enqueue(&mut b, "bad input", true, None);
    let job = b.claim(&inc, now()).unwrap().unwrap();
    b.fail(&job, "invalid input", false, now()).unwrap();
    let unfinished = enqueue(&mut b, "interrupted inference", true, None);
    let stale_job = b.claim(&inc, now()).unwrap().unwrap();
    let queued = enqueue(&mut b, "not started", true, None);
    b.renew_lease("foreground", false, now(), 60000).unwrap();
    let waiter = enqueue(&mut b, "foreground query", false, Some("foreground"));

    lose_claims(&mut b, &inc, "native process exited").unwrap();
    assert_eq!(state(&b, ack), "acknowledged");
    assert_eq!(state(&b, completed), "ready_to_index");
    assert_eq!(state(&b, permanent), "failed");
    assert_eq!(state(&b, unfinished), "retry_wait");
    assert_eq!(state(&b, queued), "queued");
    assert_eq!(state(&b, waiter), "failed");
    assert_eq!(b.job_result(completed).unwrap(), Some(vector.clone()));
    let cause: String = b
        .connection
        .query_row(
            "SELECT cause FROM waiter_failures WHERE job=?1 AND lease='foreground'",
            [waiter],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cause, "native process exited");
    drop(b);

    let mut b = Broker::open(&path, 64).unwrap();
    let new_inc = ready(&mut b);
    assert!(matches!(
        b.complete(&stale_job, &vector),
        Err(crate::Error::ObsoleteIncarnation)
    ));
    let retry = b.claim(&new_inc, now()).unwrap().unwrap();
    assert_eq!(retry.id, unfinished);
    assert_eq!(retry.attempts, stale_job.attempts + 1);
    b.complete(&retry, &vector).unwrap();
    assert_eq!(
        state(&b, permanent),
        "failed",
        "a restart is not blanket retry permission"
    );
    assert_eq!(state(&b, ack), "acknowledged");
}

#[test]
fn restart_cooldown_survives_reopen_and_leases_cannot_reset_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broker.sqlite");
    let b = Broker::open(&path, 64).unwrap();
    initialize(&b).unwrap();
    let (mut policy, _) = Recovery::load(&b, "same-config".into()).unwrap();
    for delay in [1000, 2000, 300000] {
        let before = now();
        policy.failed(&b).unwrap();
        let deadline: i64 = b
            .connection
            .query_row(
                "SELECT next_at FROM worker_recovery WHERE config='same-config'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(deadline >= before + delay && deadline <= now() + delay);
    }
    drop(b);
    let b = Broker::open(&path, 64).unwrap();
    b.renew_lease("new-client", false, now(), 60000).unwrap();
    let (policy, slot) = Recovery::load(&b, "same-config".into()).unwrap();
    assert_eq!(policy.streak, 3);
    let Slot::Waiting(until) = slot else {
        panic!("cooldown lost after restart")
    };
    assert!(until.saturating_duration_since(Instant::now()) > Duration::from_secs(290));
    let (other, _) = Recovery::load(&b, "different-config".into()).unwrap();
    assert_eq!(other.streak, 0);
}

#[test]
fn recovery_demand_is_scoped_to_live_leases_or_the_configured_models_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = Broker::open(dir.path().join("broker.sqlite"), 64).unwrap();
    initialize(&b).unwrap();
    b.connection
        .execute(
            "INSERT INTO worker_model_configs VALUES('config-a','model-a'),('config-b','model-b')",
            [],
        )
        .unwrap();
    assert!(!demand(&b, "config-a").unwrap());
    b.renew_lease("expired", false, now() - 100, 1).unwrap();
    assert!(!demand(&b, "config-a").unwrap());
    b.renew_lease("op:in-progress", true, now(), 60000).unwrap();
    assert!(demand(&b, "config-a").unwrap());
    b.release_lease("op:in-progress").unwrap();
    enqueue(&mut b, "durable indexing", true, None);
    assert!(demand(&b, "config-a").unwrap());
    assert!(
        !demand(&b, "config-b").unwrap(),
        "another model's work must not keep this model resident"
    );
}

#[test]
fn late_attempt_in_same_incarnation_cannot_replace_retry_result() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = Broker::open(dir.path().join("broker.sqlite"), 64).unwrap();
    let inc = ready(&mut b);
    enqueue(&mut b, "retryable input", true, None);
    let first = b.claim(&inc, 10).unwrap().unwrap();
    b.fail(&first, "transient failure", true, 10).unwrap();
    let second = b.claim(&inc, 1010).unwrap().unwrap();
    assert_eq!(second.attempts, 2);
    let mut old = vec![0.; DIMENSIONS];
    old[0] = 1.;
    let mut new = vec![0.; DIMENSIONS];
    new[1] = 1.;
    assert!(b.complete(&first, &old).is_err());
    b.complete(&second, &new).unwrap();
    assert_eq!(b.job_result(second.id).unwrap(), Some(new));
}
