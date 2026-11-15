//! Coordination of import review requests.
use super::stores::Stores;
use crate::{
    git::imports::ProgressRegistry, installation::registry::ConfigSnapshot, protocol::AdminCommand,
};
use agentlaw_contracts::{DomainError, RequestControl, Result};
use std::{sync::Arc, time::Instant};
mod cleanup;

type BoundaryHook = Arc<dyn Fn(&str, &str) -> Result<()> + Send + Sync>;
struct ProgressLease {
    registry: ProgressRegistry,
    reference: String,
}
impl Drop for ProgressLease {
    fn drop(&mut self) {
        let _ = self.registry.finish(&self.reference);
    }
}

pub(super) fn run(
    snapshot: &Arc<ConfigSnapshot>,
    command: AdminCommand,
    control: &RequestControl,
    stores: &Stores,
    progress: &ProgressRegistry,
) -> Result<serde_json::Value> {
    run_inner(
        snapshot,
        command,
        control,
        stores,
        progress,
        Arc::new(|_, _| Ok(())),
    )
}
fn run_inner(
    snapshot: &Arc<ConfigSnapshot>,
    command: AdminCommand,
    control: &RequestControl,
    stores: &Stores,
    progress: &ProgressRegistry,
    hook: BoundaryHook,
) -> Result<serde_json::Value> {
    let reference = match &command {
        AdminCommand::ImportPrepare { .. } => uuid::Uuid::new_v4().to_string(),
        AdminCommand::ImportInspect { import_ref }
        | AdminCommand::ImportCall { import_ref, .. }
        | AdminCommand::ImportResolve { import_ref, .. }
        | AdminCommand::ImportPublish { import_ref, .. } => import_ref.clone(),
        _ => {
            return Err(DomainError::new(
                "invalid_import_request",
                "Expected an import operation.",
            ))
        }
    };
    let tracked = matches!(
        &command,
        AdminCommand::ImportPrepare { .. } | AdminCommand::ImportResolve { .. }
    );
    let serialize = matches!(
        &command,
        AdminCommand::ImportCall { .. }
            | AdminCommand::ImportResolve { .. }
            | AdminCommand::ImportPublish { .. }
    );
    let lifetime = if serialize {
        Some(progress.begin(
            &reference,
            matches!(&command, AdminCommand::ImportPublish { .. }),
            control,
        )?)
    } else {
        None
    };
    let start = Instant::now();
    let _lease = if tracked {
        progress.update(&reference, "acquiring", 0, "items", start.elapsed())?;
        Some(ProgressLease {
            registry: progress.clone(),
            reference: reference.clone(),
        })
    } else {
        None
    };
    if matches!(&command, AdminCommand::ImportPrepare { .. }) {
        hook("prepare_started", &reference)?;
    }
    let guard = stores.acquire_for_execution(snapshot, control)?;
    let original = control.clone();
    let observed = progress.clone();
    let observed_ref = reference.clone();
    let progress_control = RequestControl::new(control.cancel.clone(), move |phase| {
        original.phase(phase);
        if tracked {
            let _ = observed.update(&observed_ref, phase, 0, "items", start.elapsed());
        }
    });
    match command {
        AdminCommand::ImportPrepare { commit } => {
            cleanup::completed(stores, snapshot, &guard, progress, &progress_control);
            progress_control.check()?;
            let value = guard.import_prepare(&commit, &reference, &progress_control)?;
            drop(_lease);
            hook("prepare_completed", &reference)?;
            Ok(value)
        }
        AdminCommand::ImportInspect { import_ref } => {
            guard.import_inspect(&import_ref, &progress_control)
        }
        AdminCommand::ImportResolve {
            import_ref,
            choices,
            user_confirmed,
        } => {
            if let Some(choices) = choices {
                let stage = guard.import_stage(&import_ref, false)?;
                let staged = stores.acquire_stage(snapshot, &stage, &progress_control)?;
                crate::store::import_stage::apply_choices(
                    &staged,
                    &stage,
                    choices,
                    user_confirmed,
                    &progress_control,
                )?;
            }
            guard.import_resolve(&import_ref, &progress_control)
        }
        AdminCommand::ImportPublish {
            import_ref,
            resolution,
            user_confirmed,
        } => {
            let stage = guard.import_stage(&import_ref, true)?;
            hook("publish_guard_held", &reference)?;
            let result = guard.import_publish_observed(
                &import_ref,
                &resolution,
                user_confirmed,
                &progress_control,
                &|| hook("canonical_published", &reference),
            )?;
            if let Some(lifetime) = &lifetime {
                lifetime.completed();
            }
            stores.close_stage(snapshot, &stage, &RequestControl::default())?;
            Ok(result)
        }
        AdminCommand::ImportCall {
            import_ref,
            request,
        } => {
            let stage = guard.import_stage(&import_ref, false)?;
            let staged = stores.acquire_stage(snapshot, &stage, &progress_control)?;
            hook("staged_call_guard_held", &reference)?;
            crate::store::import_stage::call(&staged, snapshot, request, &progress_control)
        }
        _ => unreachable!(),
    }
}
#[cfg(test)]
fn run_observed(
    snapshot: &ConfigSnapshot,
    command: AdminCommand,
    control: &RequestControl,
    stores: &Stores,
    progress: &ProgressRegistry,
    hook: BoundaryHook,
) -> Result<serde_json::Value> {
    run_inner(
        &Arc::new(snapshot.clone()),
        command,
        control,
        stores,
        progress,
        hook,
    )
}
#[cfg(test)]
mod tests;
