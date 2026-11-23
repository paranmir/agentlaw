//! Best-effort shutdown for runtimes started in test-owned installations.
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub struct RuntimeCleanup(pub PathBuf);

impl RuntimeCleanup {
    pub fn shutdown(&self) {
        if !self.0.join("service").exists() {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(endpoint) = agentlaw_runtime::transport::read_endpoint(&self.0) {
                if let Ok(client) =
                    agentlaw_runtime::client::Client::connect(&self.0, &endpoint.installation_build)
                {
                    let _ = client.shutdown("test cleanup");
                }
            }
            if !agentlaw_runtime::installation::owner::owner_alive(&self.0).unwrap_or(false)
                || Instant::now() >= deadline
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for RuntimeCleanup {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Own one managed-update fixture child, including panic cleanup.
#[allow(dead_code)]
pub struct OwnedStaticChild(pub std::process::Child);

impl Drop for OwnedStaticChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
