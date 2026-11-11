//! Runtime process lifetime, acceptance and component supervision.

pub mod accept;
pub mod dispatch;
pub mod health;
pub mod lifecycle;
pub mod mailbox;
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

pub fn run(deps: crate::ports::HostDeps) -> crate::ports::RuntimeResult<RunExit> {
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
        ports::{ComponentId, ComponentKind, ComponentState, HealthSink, RuntimeError},
        projection::ProjectionConfig,
        semantic::{SemanticConfig, SemanticIndexer, SemanticSearch, INDEX_CONFIG_VERSION},
        store::service::StoreDeps,
    };
    use agentlaw_worker::supervisor::ModelRequestIds;
    use std::{sync::Arc, time::Duration};

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
    let assets = (deps.assets)().unwrap_or(None);
    let worker = agentlaw_worker::RuntimeConfig {
        state_dir: deps.state.join("worker"),
        executable: deps.executable.clone(),
        model: assets.clone(),
    };
    let supervisor = supervision::Supervisor::new(health.clone());
    if assets.is_some() {
        let worker_config = worker.clone();
        let worker_indexer = indexer.clone();
        let worker_ids = ids.clone();
        let worker_health = health.clone();
        let worker_model = model.clone();
        supervisor.spawn(
            ComponentId {
                binding: None,
                kind: ComponentKind::Model,
            },
            move |stop| {
                model::run_model_component(
                    worker_config.clone(),
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
        worker,
    ));
    let warm_stores = stores.clone();
    let warm_registry = registry.clone();
    let warm_model = model.clone();
    let lifecycle = lifecycle::Lifecycle::system(Arc::new(move |warm| {
        warm_model.set_warm(warm);
        if warm {
            let stores = warm_stores.clone();
            let registry = warm_registry.clone();
            std::thread::spawn(move || {
                if let ConfigState::Ready(snapshot) = registry.current() {
                    stores.prewarm(&snapshot);
                }
            });
        }
    }));
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
