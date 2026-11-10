use super::*;

#[test]
fn completed_installation_journal_retains_update_owner() {
    for already_owned in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path();
        let target = state.join("configuration");
        fs::write(&target, "before").unwrap();
        let owner = uuid::Uuid::new_v4().to_string();
        let journal = InstallJournal {
            targets: vec![Replacement {
                path: target.clone(),
                before: Some("before".into()),
                after: "after".into(),
            }],
            update_plan_id: already_owned.then(|| owner.clone()),
        };
        fs::write(
            state.join("install-pending.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        recover_journal(state, Some(&owner)).unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "after");
        let completed = fs::read_dir(state)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("install-completed-")
            })
            .unwrap();
        let completed: InstallJournal =
            serde_json::from_slice(&fs::read(completed).unwrap()).unwrap();
        assert_eq!(completed.update_plan_id.as_deref(), Some(owner.as_str()));
        assert!(!state.join("install-pending.json").exists());
    }
}
#[test]
fn generated_bootstrap_matches_accepted_contract() {
    let guidance = super::super::GUIDANCE.replace("\r\n", "\n");
    let accepted = guidance
        .split_once("## Bootstrap")
        .unwrap()
        .1
        .split_once("```text\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    let expected = accepted
        .replace(
            "{state_assignment}",
            if cfg!(windows) {
                "$env:AGENTLAW_HOME='state'"
            } else {
                "AGENTLAW_HOME='state'"
            },
        )
        .replace(
            "{invocation}",
            if cfg!(windows) {
                "& 'agentlaw'"
            } else {
                "'agentlaw'"
            },
        );
    let generated = bootstrap(Path::new("agentlaw"), Path::new("state"));
    let body = generated
        .split_once("## Agentlaw memory\n\n")
        .unwrap()
        .1
        .split_once(&format!("\n{END}"))
        .unwrap()
        .0;
    assert_eq!(body, expected);
}
#[test]
fn instruction_replacement_preserves_user_text_and_refuses_ambiguous_markers() {
    let first = upsert_bootstrap("User rules\n", &format!("{BEGIN}\nold\n{END}")).unwrap();
    let second = upsert_bootstrap(&first, &format!("{BEGIN}\nnew\n{END}")).unwrap();
    assert!(same_owned_bootstrap(
        &second,
        &format!("Unrelated user rule\n{second}")
    ));
    assert!(!same_owned_bootstrap(
        &second,
        &second.replace("new", "changed")
    ));
    assert!(!same_owned_bootstrap(
        &second,
        &format!("{second}\n{BEGIN}")
    ));
    assert!(second.starts_with("User rules\n"));
    assert_eq!(second.matches(BEGIN).count(), 1);
    assert!(!second.contains("old"));
    assert!(upsert_bootstrap(BEGIN, "x").is_err());
}
#[test]
fn config_preserves_other_servers_and_requires_ownership() {
    let d = tempfile::tempdir().unwrap();
    let exe = d.path().join("app.exe");
    let old = "model = 'keep'\n[mcp_servers.other]\ncommand = 'other'\n";
    let text = configuration(Harness::Codex, old, &exe, d.path(), false).unwrap();
    let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(doc["model"].as_str(), Some("keep"));
    assert_eq!(
        doc["mcp_servers"]["other"]["command"].as_str(),
        Some("other")
    );
    assert!(configuration(Harness::Codex, &text, &exe, d.path(), false).is_err());
    assert!(configuration(Harness::Codex, &text, &exe, d.path(), true).is_ok());
    let json = configuration(
        Harness::OhMyPi,
        "{\"mcpServers\":{\"other\":{\"command\":\"keep\"}}}",
        &exe,
        d.path(),
        false,
    )
    .unwrap();
    let v: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["mcpServers"]["other"]["command"], "keep");
}
#[test]
fn interrupted_install_resumes_without_overwriting_new_user_edits() {
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("AGENTS.md");
    fs::write(&target, "before").unwrap();
    atomic_text(
        &d.path().join("install-pending.json"),
        &serde_json::to_string(&InstallJournal {
            targets: vec![Replacement {
                path: target.clone(),
                before: Some("before".into()),
                after: "after".into(),
            }],
            update_plan_id: None,
        })
        .unwrap(),
    )
    .unwrap();
    fs::write(&target, "user changed").unwrap();
    assert!(recover_journal(d.path(), None).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "user changed");
    fs::write(&target, "before").unwrap();
    recover_journal(d.path(), None).unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "after");
    assert!(!d.path().join("install-pending.json").exists());
}
