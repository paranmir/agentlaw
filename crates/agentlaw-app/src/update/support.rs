//! Independent, interactive GitHub support. No token enters Agentlaw state.
use crate::config;
use agentlaw_contracts::{DomainError, Result};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    process::Command,
};

const REPOSITORY: &str = "paranmir/agentlaw";
const ENDPOINT: &str = "user/starred/paranmir/agentlaw";

fn error(message: &str) -> DomainError {
    DomainError::new("github_support_unavailable", message)
}

fn gh(args: &[&str]) -> Result<std::process::Output> {
    Command::new("gh")
        .args(args)
        .output()
        .map_err(|_| error("GitHub CLI authentication is unavailable; no star was changed."))
}

fn identity() -> Result<(u64, String)> {
    let output = gh(&["api", "user"])?;
    if !output.status.success() {
        return Err(error(
            "GitHub CLI is not authenticated; no star was changed.",
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| error("GitHub account identity could not be verified."))?;
    let id = value["id"]
        .as_u64()
        .ok_or_else(|| error("GitHub account ID could not be verified."))?;
    let login = value["login"]
        .as_str()
        .ok_or_else(|| error("GitHub account name could not be verified."))?;
    Ok((id, login.to_owned()))
}

fn status_code(output: &std::process::Output) -> Option<u16> {
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let protocol = fields.next()?;
        if protocol.starts_with("HTTP/") {
            fields.next()?.parse().ok()
        } else {
            None
        }
    })
}

fn dismissed(account: u64) -> bool {
    let Ok(state) = config::state_root() else {
        return false;
    };
    let Ok(bytes) = fs::read(state.join("support-star.json")) else {
        return false;
    };
    if bytes.len() > 4096 {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    value["dismissed"].as_array().is_some_and(|entries| {
        entries.iter().any(|entry| {
            entry["host"] == "github.com"
                && entry["account_id"] == account
                && entry["repository"] == REPOSITORY
        })
    })
}

fn record_dismissal(account: u64) -> Result<()> {
    let state = config::state_root()?;
    fs::create_dir_all(&state)
        .map_err(|_| error("Cannot create local support preference state."))?;
    let path = state.join("support-star.json");
    let mut value = fs::read(&path)
        .ok()
        .filter(|bytes| bytes.len() <= 4096)
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .unwrap_or_else(|| json!({"dismissed":[]}));
    let entries = value["dismissed"]
        .as_array_mut()
        .ok_or_else(|| error("The local support preference state is invalid."))?;
    let entry = json!({"host":"github.com","account_id":account,"repository":REPOSITORY});
    if !entries.contains(&entry) {
        entries.push(entry);
    }
    let mut temp = tempfile::NamedTempFile::new_in(&state)
        .map_err(|_| error("Cannot stage local support preference state."))?;
    let bytes = serde_json::to_vec(&value)
        .map_err(|_| error("Cannot encode local support preference state."))?;
    temp.write_all(&bytes)
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|_| error("Cannot persist local support preference state."))?;
    temp.persist(path)
        .map_err(|_| error("Cannot publish local support preference state."))?;
    Ok(())
}

pub fn star(ask_again: bool) -> Result<Value> {
    let (account, login) = identity()?;
    let check = gh(&["api", "--include", ENDPOINT])?;
    match status_code(&check) {
        Some(204) => {
            return Ok(json!({"status":"already_starred","account":login,"repository":REPOSITORY}))
        }
        Some(404) => {}
        _ => {
            return Ok(
                json!({"status":"unknown","account":login,"repository":REPOSITORY,
            "reason":"Star status could not be verified with this account and its permissions."}),
            )
        }
    }
    if !ask_again && dismissed(account) {
        return Ok(
            json!({"status":"dismissed","account":login,"repository":REPOSITORY,
            "next_action":"Run agentlaw support star --ask-again if you want to reconsider."}),
        );
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(
            json!({"status":"skipped_noninteractive","account":login,"repository":REPOSITORY}),
        );
    }
    eprint!("GitHub account {login}: star {REPOSITORY}? (y/N) ");
    io::stderr()
        .flush()
        .map_err(|_| error("Could not show the star prompt."))?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|_| error("Could not read the star choice."))?;
    match answer.trim() {
        "y" | "Y" => {
            let (still_account, _) = identity()?;
            if still_account != account {
                return Err(error(
                    "The authenticated GitHub account changed during consent; no star was changed.",
                ));
            }
            let write = gh(&[
                "api",
                "--include",
                "--method",
                "PUT",
                "--header",
                "Content-Length: 0",
                ENDPOINT,
            ])?;
            if status_code(&write) == Some(204) && write.status.success() {
                Ok(json!({"status":"starred","account":login,"repository":REPOSITORY}))
            } else {
                Ok(
                    json!({"status":"unknown","account":login,"repository":REPOSITORY,
                    "reason":"GitHub did not confirm the star operation."}),
                )
            }
        }
        "n" | "N" => {
            record_dismissal(account)?;
            Ok(json!({"status":"declined","account":login,"repository":REPOSITORY}))
        }
        _ => Ok(json!({"status":"skipped","account":login,"repository":REPOSITORY})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_parser_keeps_404_distinct_from_auth_or_permission_errors() {
        fn output(code: &str) -> std::process::Output {
            std::process::Output {
                status: Command::new("rustc").arg("--version").status().unwrap(),
                stdout: format!("HTTP/2.0 {code}\r\n\r\n").into_bytes(),
                stderr: vec![],
            }
        }
        assert_eq!(status_code(&output("204 No Content")), Some(204));
        assert_eq!(status_code(&output("404 Not Found")), Some(404));
        assert_eq!(status_code(&output("403 Forbidden")), Some(403));
    }
}
