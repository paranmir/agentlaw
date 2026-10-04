//! Best-effort release advice is outside the memory runtime.
pub mod managed;
pub mod support;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
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

fn read_cache(state: &Path) -> Cache {
    let path = state.join("update-check.json");
    let Ok(bytes) = fs::read(path) else {
        return Cache::default();
    };
    if bytes.len() > 4096 {
        return Cache::default();
    }
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn write_cache(state: &Path, cache: &Cache) {
    if fs::create_dir_all(state).is_err() {
        return;
    }
    let Ok(mut temp) = tempfile::NamedTempFile::new_in(state) else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(cache) else {
        return;
    };
    if temp.write_all(&bytes).is_ok() && temp.as_file().sync_all().is_ok() {
        let _ = temp.persist(state.join("update-check.json"));
    }
}

impl Advisor {
    pub fn maintenance_path(&self) -> PathBuf {
        self.state.join("update-maintenance.json")
    }
    pub fn new(state: PathBuf) -> Self {
        let current = read_cache(&state);
        let advisor = Self {
            state,
            current: Arc::new(Mutex::new(current)),
            running: Arc::new(Mutex::new(false)),
        };
        advisor.schedule();
        advisor
    }

    pub fn schedule(&self) {
        let current = self.current.clone();
        let running = self.running.clone();
        let state = self.state.clone();
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
            let _ = fs::create_dir_all(&state);
            let lock = fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(state.join("update-check.lock"));
            let Ok(lock) = lock else {
                *running.lock().unwrap_or_else(|e| e.into_inner()) = false;
                return;
            };
            if lock.lock_exclusive().is_err() {
                *running.lock().unwrap_or_else(|e| e.into_inner()) = false;
                return;
            }
            let disk = read_cache(&state);
            if disk
                .last_success
                .is_some_and(|at| at.saturating_add(SUCCESS_TTL) > now())
                || disk.retry_after.is_some_and(|until| until > now())
            {
                *current.lock().unwrap_or_else(|e| e.into_inner()) = disk;
                *running.lock().unwrap_or_else(|e| e.into_inner()) = false;
                return;
            }
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
            write_cache(&state, &cache);
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
mod tests {
    use super::*;
    #[test]
    fn only_full_published_versions_are_admitted() {
        assert_eq!(version("v0.2.7"), Some([0, 2, 7]));
        assert_eq!(version("v0.2.7-rc1"), None);
        assert_eq!(version("v0.2.7.1"), None);
        assert_eq!(
            parse_release(br#"{"tag_name":"v0.2.7","draft":false,"prerelease":false}"#),
            Some("v0.2.7".into())
        );
        assert_eq!(
            parse_release(br#"{"tag_name":"v0.2.7-rc1","draft":false,"prerelease":false}"#),
            None
        );
        assert_eq!(
            parse_release(br#"{"tag_name":"v0.2.8","draft":true,"prerelease":false}"#),
            None
        );
    }

    #[test]
    fn invalid_cache_is_unknown_and_does_not_block_advice() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("update-check.json"), b"not json").unwrap();
        assert!(read_cache(root.path()).latest_tag.is_none());
        let cache = Cache {
            last_success: Some(now()),
            retry_after: None,
            latest_tag: Some("v99.0.0".into()),
        };
        write_cache(root.path(), &cache);
        let advisor = Advisor::new(root.path().to_path_buf());
        let notice = advisor.notice().unwrap();
        assert_eq!(notice["latest_version"], "v99.0.0");
        let next_action = notice["next_action"].as_str().unwrap();
        assert!(next_action.contains("After explicit user approval"));
        assert!(next_action.contains("installed public agentlaw update"));
        assert!(!next_action.contains("preview"));
    }

    #[test]
    fn conditional_guidance_matches_build_consumed_contract() {
        let guidance =
            include_str!("../../../docs/contracts/agentlaw-llm-guidance.md").replace("\r\n", "\n");
        let section = guidance
            .split_once("## Conditional release advisory in a tool result")
            .unwrap()
            .1;
        let body = section
            .split_once("```text\n")
            .unwrap()
            .1
            .split_once("\n```")
            .unwrap()
            .0;
        // Git checkout line endings may occur inside this multiline fence.
        // Compare the complete wording with the same normalization on both sides.
        assert_eq!(body, crate::update_notice_guidance().replace("\r\n", "\n"));
    }

    #[test]
    fn ordinary_notice_ignores_update_plan_history() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        fs::create_dir_all(state.join("update-plans")).unwrap();
        for index in 0..70 {
            fs::write(
                state.join("update-plans").join(format!("{index}.json")),
                b"corrupt historical plan",
            )
            .unwrap();
        }
        write_cache(
            &state,
            &Cache {
                last_success: Some(now()),
                retry_after: None,
                latest_tag: Some(format!("v{}", env!("CARGO_PKG_VERSION"))),
            },
        );
        assert!(Advisor::new(state).notice().is_none());
    }
}
