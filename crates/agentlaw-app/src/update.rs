//! Best-effort release advice is outside the memory runtime.
pub mod managed;
pub mod support;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

const RELEASE_API: &str = "https://api.github.com/repos/paranmir/agentlaw/releases/latest";
const RELEASE_PAGE: &str = "https://github.com/paranmir/agentlaw/releases/tag/";
const SUCCESS_TTL: u64 = 24 * 60 * 60;
const FAILURE_RETRY: u64 = 60 * 60;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Cache {
    last_success: Option<u64>,
    retry_after: Option<u64>,
    latest_tag: Option<String>,
}

#[derive(Clone)]
pub struct Advisor {
    state: PathBuf,
    current: Arc<Mutex<Cache>>,
    running: Arc<Mutex<bool>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn version(tag: &str) -> Option<[u64; 3]> {
    let mut parts = tag.strip_prefix('v').unwrap_or(tag).split('.');
    let value = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ];
    parts.next().is_none().then_some(value)
}

fn parse_release(bytes: &[u8]) -> Option<String> {
    let release: Value = serde_json::from_slice(bytes).ok()?;
    if release["draft"] != false || release["prerelease"] != false {
        return None;
    }
    let tag = release["tag_name"].as_str()?;
    version(tag)?;
    tag.starts_with('v').then(|| tag.to_owned())
}

fn curl(url: &str) -> Option<Vec<u8>> {
    let output = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-time",
            "5",
            "--max-filesize",
            "65536",
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "User-Agent: agentlaw-update-advisor",
            url,
        ])
        .output()
        .ok()?;
    (output.status.success() && output.stdout.len() <= 65536).then_some(output.stdout)
}

fn fetch() -> Option<String> {
    parse_release(&curl(RELEASE_API)?)
}

impl Advisor {
    pub fn maintenance_path(&self) -> PathBuf {
        agentlaw_runtime::installation::gate::path(&self.state)
    }
    pub fn new(state: PathBuf) -> Self {
        let advisor = Self {
            state,
            current: Arc::new(Mutex::new(Cache::default())),
            running: Arc::new(Mutex::new(false)),
        };
        advisor.schedule();
        advisor
    }

    pub fn schedule(&self) {
        let current = self.current.clone();
        let running = self.running.clone();
        let time = now();
        let cache = current.lock().unwrap_or_else(|e| e.into_inner());
        if cache.retry_after.is_some_and(|until| until > time)
            || cache
                .last_success
                .is_some_and(|at| at.saturating_add(SUCCESS_TTL) > time)
        {
            return;
        }
        drop(cache);
        let mut busy = running.lock().unwrap_or_else(|e| e.into_inner());
        if *busy {
            return;
        }
        *busy = true;
        drop(busy);
        std::thread::spawn(move || {
            let latest = fetch();
            let mut cache = current.lock().unwrap_or_else(|e| e.into_inner());
            match latest {
                Some(tag) => {
                    cache.latest_tag = Some(tag);
                    cache.last_success = Some(now());
                    cache.retry_after = None;
                }
                None => cache.retry_after = Some(now().saturating_add(FAILURE_RETRY)),
            }
            *running.lock().unwrap_or_else(|e| e.into_inner()) = false;
        });
    }

    pub fn notice(&self) -> Option<Value> {
        self.schedule();
        let cache = self.current.lock().unwrap_or_else(|e| e.into_inner());
        let tag = cache.latest_tag.as_deref()?;
        let latest = version(tag)?;
        let running = version(env!("CARGO_PKG_VERSION"))?;
        (latest > running).then(|| {
            json!({"kind":"new_release","running_version":env!("CARGO_PKG_VERSION"),
                "latest_version":tag,"release_url":format!("{RELEASE_PAGE}{tag}"),
                "last_verified_at":cache.last_success,
                "verification":if cache.last_success.is_some_and(|at|at.saturating_add(SUCCESS_TTL)>now()) {"fresh"} else {"stale"},
                "next_action":"After explicit user approval, run the installed public agentlaw update for the complete managed update.",
                "guidance":crate::update_notice_guidance()})
        })
    }
}

pub fn check() -> Value {
    match fetch() {
        Some(tag) => {
            let current = env!("CARGO_PKG_VERSION");
            let available = version(&tag)
                .zip(version(current))
                .is_some_and(|(a, b)| a > b);
            json!({"status":"checked","running_version":current,"latest_version":tag,
                "update_available":available,"release_url":format!("{RELEASE_PAGE}{tag}")})
        }
        None => json!({"status":"unknown","running_version":env!("CARGO_PKG_VERSION"),
            "reason":"The latest published release could not be verified."}),
    }
}

#[cfg(test)]
mod tests;
