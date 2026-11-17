//! Runtime process lifetime, acceptance and component supervision.

pub mod accept;
pub mod dispatch;
pub mod health;
mod imports;
pub mod lifecycle;
pub mod mailbox;
mod maintenance;
mod store_dispatch;
mod stores;
pub mod supervision;

#[cfg(test)]
mod tests;

pub enum RunExit {
    Stopped,
    AlreadyOwned,
    StartBlocked(agentlaw_contracts::DomainError),
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn prepare_test_selection(
    state: &std::path::Path,
    path: &std::path::Path,
    create: bool,
) -> agentlaw_contracts::Result<serde_json::Value> {
    use crate::{
        installation::{config, registry::ConfigRegistry, setup},
        model::ModelHandle,
        ports::HealthSink,
        projection::ProjectionConfig,
        semantic::{SemanticConfig, SemanticIndexer, SemanticSearch, INDEX_CONFIG_VERSION},
        store::service::StoreDeps,
    };
    use std::{sync::Arc, time::Duration};
    let health = HealthSink::default();
    let indexer = SemanticIndexer::start(
        SemanticConfig::new(state.join("worker/indices"), INDEX_CONFIG_VERSION.into()),
        agentlaw_worker::supervisor::ModelRequestIds::new(),
        health.clone(),
    )
    .map_err(|e| agentlaw_contracts::DomainError::new("store_unavailable", e.to_string()))?;
    let registry = Arc::new(ConfigRegistry::new());
    registry.publish(store_dispatch::read_config(state));
    let stores = stores::Stores::new(
        state.to_owned(),
        StoreDeps {
            health,
            semantic: Arc::new(SemanticSearch::new(indexer.clone(), ModelHandle::new())),
            indexer: indexer.clone(),
            projection: ProjectionConfig::new(),
            coordination: config::coordination_root(state),
            state: state.to_owned(),
        },
        registry.clone(),
    );
    let result = setup::connect_prepared(
        state,
        path,
        create,
        &agentlaw_contracts::RequestControl::default(),
        &registry,
        |snapshot, control| {
            stores
                .acquire(snapshot)
                .map_err(|e| {
                    agentlaw_contracts::DomainError::new("store_unavailable", format!("{e:?}"))
                })?
                .prepare_selection(control, false)
        },
    )
    .map(|(value, _)| value);
    stores.close_all();
    indexer.shutdown(Duration::from_secs(30));
    result
}

pub fn run(deps: crate::ports::HostDeps) -> crate::ports::RuntimeResult<RunExit> {
    run_with_clock(deps, std::sync::Arc::new(std::time::Instant::now))
}

/// Runs the host with the idle and lease clock supplied by the caller; tests use it to advance time.
fn run_with_clock(
    deps: crate::ports::HostDeps,
    clock: lifecycle::Clock,
) -> crate::ports::RuntimeResult<RunExit> {
    let Some(_owner) = crate::installation::owner::OwnerLock::try_acquire(&deps.state)
        .map_err(|e| crate::ports::RuntimeError::Unavailable(e.to_string()))?
    else {
        return Ok(RunExit::AlreadyOwned);
    };
    if let Err(error) = (deps.start_gate)() {
        return Ok(RunExit::StartBlocked(error));
    }
    match std::fs::remove_file(deps.state.join("service/endpoint.json")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(crate::ports::RuntimeError::Unavailable(error.to_string())),
    }
    use crate::{
        installation::{
            config,
            registry::{ConfigRegistry, ConfigState},
        },
        model::{self, ModelHandle},
        ports::{
            ActivityKind, ComponentId, ComponentKind, ComponentState, HealthSink, RuntimeError,
        },
        projection::ProjectionConfig,
        semantic::{SemanticConfig, SemanticIndexer, SemanticSearch, INDEX_CONFIG_VERSION},
        store::service::StoreDeps,
    };
    use agentlaw_worker::supervisor::ModelRequestIds;
    use std::{
        sync::{Arc, Mutex, OnceLock},
        time::Duration,
    };

    // UUID version and variant bits are omitted; the remaining bytes come from the OS RNG.
    let mut random = Vec::with_capacity(42);
    for _ in 0..3 {
        let id = uuid::Uuid::new_v4();
        let bytes = id.as_bytes();
        random.extend_from_slice(&bytes[..6]);
        random.push(bytes[7]);
        random.extend_from_slice(&bytes[9..]);
    }
    let secret: String = random[..32].iter().map(|b| format!("{b:02x}")).collect();
    let registry = Arc::new(ConfigRegistry::new());
    registry.publish(store_dispatch::read_config(&deps.state));
    let health = HealthSink::default();
    health::publish_host(&health, ComponentState::Starting, None);
    let ids = ModelRequestIds::new();
    let model = ModelHandle::new();
    let indexer = SemanticIndexer::start(
        SemanticConfig::new(
            deps.state.join("worker").join("indices"),
            INDEX_CONFIG_VERSION.into(),
        ),
        ids.clone(),
        health.clone(),
    )?;
    let wake_model = model.clone();
    indexer.set_model_waker(Arc::new(move || wake_model.wake()));
    let semantic = Arc::new(SemanticSearch::new(indexer.clone(), model.clone()));
    let initial_assets = (deps.assets)();
    let worker = agentlaw_worker::RuntimeConfig {
        state_dir: deps.state.join("worker"),
        executable: deps.executable.clone(),
        model: initial_assets.as_ref().ok().cloned().flatten(),
    };
    let supervisor = supervision::Supervisor::new(health.clone());
    if !matches!(&initial_assets, Ok(None)) {
        let worker_config = worker.clone();
        let worker_indexer = indexer.clone();
        let worker_ids = ids.clone();
        let worker_health = health.clone();
        let worker_model = model.clone();
        let assets = deps.assets.clone();
        let initial = Mutex::new(Some(initial_assets));
        supervisor.spawn(
            ComponentId {
                binding: None,
                kind: ComponentKind::Model,
            },
            move |stop| {
                let next = initial
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .unwrap_or_else(|| assets());
                let Some(assets) = next.map_err(|error| format!("asset: {error}"))? else {
                    worker_health.remove(&ComponentId {
                        binding: None,
                        kind: ComponentKind::Model,
                    });
                    while !stop.is_stopping() {
                        stop.wait(Duration::from_secs(1));
                    }
                    return Ok(());
                };
                let mut config = worker_config.clone();
                config.model = Some(assets);
                model::run_model_component(
                    config,
                    Arc::new(worker_indexer.clone()),
                    worker_ids.clone(),
                    worker_health.clone(),
                    worker_model.clone(),
                    stop,
                )
            },
        )?;
    }
    let stores = Arc::new(stores::Stores::new(
        deps.state.clone(),
        StoreDeps {
            health: health.clone(),
            semantic,
            indexer: indexer.clone(),
            projection: ProjectionConfig::new(),
            coordination: config::coordination_root(&deps.state),
            state: deps.state.clone(),
        },
        registry.clone(),
    ));
    let dispatcher = Arc::new(store_dispatch::StoreDispatch::new(
        deps.state.clone(),
        registry.clone(),
        stores.clone(),
        deps.assets.clone(),
    ));
    let warm_stores = stores.clone();
    let warm_registry = registry.clone();
    let warm_model = model.clone();
    let lifecycle_ref = Arc::new(OnceLock::<lifecycle::Lifecycle>::new());
    let warm_lifecycle = Arc::downgrade(&lifecycle_ref);
    let lifecycle = lifecycle::Lifecycle::new(
        clock,
        Arc::new(move |warm| {
            warm_model.set_warm(warm);
            if warm {
                let Some(guard) = warm_lifecycle.upgrade().and_then(|cell| {
                    cell.get()
                        .and_then(|l| l.begin(ActivityKind::Execution).ok())
                }) else {
                    return;
                };
                let stores = warm_stores.clone();
                let registry = warm_registry.clone();
                std::thread::spawn(move || {
                    let _guard = guard;
                    if let ConfigState::Ready(snapshot) = registry.current() {
                        stores.prewarm(&snapshot);
                    }
                });
            }
        }),
    );
    let _ = lifecycle_ref.set(lifecycle.clone());
    let stopping_stores = stores.clone();
    lifecycle.set_on_stopping(Arc::new(move || stopping_stores.stop_opening()));
    let listener = accept::start(
        &deps.state,
        &deps.installation_build,
        &secret,
        dispatcher,
        registry,
        health.clone(),
        lifecycle.clone(),
        accept::HostLimits::default(),
    )
    .map_err(|error| RuntimeError::Unavailable(error.to_string()))?;
    health::publish_host(&health, ComponentState::Ready, None);
    loop {
        if lifecycle.poll().state != lifecycle::LifecycleState::Running {
            if lifecycle.finish(
                || stores.close_all(),
                || {
                    supervisor.shutdown();
                    indexer.shutdown(Duration::from_secs(30));
                },
            ) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    drop(listener);
    match std::fs::remove_file(deps.state.join("service/endpoint.json")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(RuntimeError::Unavailable(error.to_string())),
    }
    drop(_owner);
    Ok(RunExit::Stopped)
}
