//! Runtime process lifetime, acceptance and component supervision.

pub mod accept;
pub mod dispatch;
pub mod health;
pub mod lifecycle;
pub mod mailbox;
pub mod supervision;

#[cfg(test)]
mod tests;

pub enum RunExit {
    Stopped,
    AlreadyOwned,
    StartBlocked(agentlaw_contracts::DomainError),
}

pub fn run(_deps: crate::ports::HostDeps) -> crate::ports::RuntimeResult<RunExit> {
    Err(crate::ports::RuntimeError::Unavailable(
        "host is not implemented".into(),
    ))
}
