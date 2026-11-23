use super::*;

struct NoSpawn;

impl WorkerProcessPort for NoSpawn {
    fn spawn(&self, _: &RuntimeConfig) -> Result<Child> {
        panic!("locked model must prevent spawning")
    }

    fn observe(
        &self,
        _: &Child,
        _: String,
        _: mpsc::Sender<Event>,
        _: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<()> {
        unreachable!()
    }

    fn stop(&self, _: &mut Child) -> Result<()> {
        unreachable!()
    }

    fn control(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<u64>> {
        unreachable!()
    }
}

#[test]
fn held_model_lock_prevents_spawn_and_schedules_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join("model.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let config = RuntimeConfig {
        state_dir: dir.path().into(),
        executable: "unused".into(),
        model: None,
    };
    let (events, _) = mpsc::channel();
    let signal = Arc::new(RamSignal {
        events: events.clone(),
    });
    let now = Instant::now();
    let error = match start_ram_child(&NoSpawn, &config, &events, &signal, now, None) {
        Ok(_) => panic!("held lock must prevent start"),
        Err(error) => error,
    };
    assert_eq!(error.cause, "previous model child still running");
    let mut lifecycle = ModelLifecycle::default();
    lifecycle.exited(now, error.cause);
    assert_eq!(lifecycle.action(now, true, true), LifecycleAction::None);
    assert_eq!(
        lifecycle.action(now + Duration::from_secs(1), true, true),
        LifecycleAction::Start
    );
}
