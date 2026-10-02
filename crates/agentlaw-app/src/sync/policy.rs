use super::*;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationPolicy {
    pub policy_id: String,
    pub version: u64,
    pub enabled: bool,
    pub source: PathBuf,
    pub control: PathBuf,
    pub remote: String,
    pub fetch_endpoint: String,
    pub push_endpoint: String,
    pub target_ref: String,
    pub allowed_operations: Vec<String>,
    pub allowed_scopes: Vec<String>,
    pub push_permission: bool,
}
pub(super) fn digest(policy: &DelegationPolicy) -> String {
    git::hash(&serde_json::to_vec(policy).unwrap())
}
pub fn propose_policy(
    store: &Store,
    local: &Path,
    remote: &str,
    target_ref: &str,
) -> Result<DelegationPolicy> {
    let source = store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .map_err(storage_error)?;
    git::git_root(&source)?;
    git::run(&source, &["check-ref-format", target_ref])?;
    if !target_ref.starts_with("refs/heads/") {
        return Err(DomainError::new(
            "invalid_target_ref",
            "Select an explicit branch destination.",
        ));
    }
    let fetch = git::text(&source, &["remote", "get-url", "--all", remote])?;
    if fetch.lines().count() != 1 {
        return Err(DomainError::new(
            "ambiguous_destination",
            "Exactly one actual fetch endpoint is required.",
        ));
    }
    Ok(DelegationPolicy {
        policy_id: uuid::Uuid::new_v4().to_string(),
        version: 1,
        enabled: false,
        source,
        control: fs::canonicalize(local).map_err(|_| git::io_error("policy control binding"))?,
        remote: remote.into(),
        fetch_endpoint: fetch,
        push_endpoint: git::endpoint(
            &store
                .with_source_read(|p, _| Ok(p.to_path_buf()))
                .map_err(storage_error)?,
            remote,
        )?,
        target_ref: target_ref.into(),
        allowed_operations: vec![
            "memory".into(),
            "procedure".into(),
            "redirect".into(),
            "catalog".into(),
        ],
        allowed_scopes: vec![
            "user".into(),
            "machine".into(),
            "project".into(),
            "project_machine".into(),
        ],
        push_permission: true,
    })
}
/// Explicit local CLI configuration, deliberately absent from the sync MCP action.
pub fn configure_policy(
    state: &Path,
    store: &Store,
    local: &Path,
    file: &Path,
    confirmed: bool,
) -> Result<Value> {
    if !confirmed {
        return Err(DomainError::new("delegation_confirmation_required","The user must explicitly activate this continuing local delegation. It is not per-solution approval."));
    }
    let policy: DelegationPolicy = read_json(file)?;
    check_bindings(store, local, &policy)?;
    let dir = state.join("policies");
    fs::create_dir_all(&dir).map_err(|_| git::io_error("policy directory"))?;
    git::save_local_json(
        &dir.join(format!("sync-{}.json", policy.policy_id)),
        &policy,
    )?;
    Ok(
        json!({"policy_id":policy.policy_id,"enabled":policy.enabled,"version":policy.version,"authority_kind":"local_policy","limitations":"This is continuing OS-local delegation, not a signed per-call user approval. It does not protect against malicious tools running as the same OS user."}),
    )
}
pub(super) fn load(state: &Path, id: &str) -> Result<DelegationPolicy> {
    agentlaw_storage::validate_id(id).map_err(|_| {
        DomainError::new(
            "invalid_policy_id",
            "Copy a registered policy ID; policy paths/overrides are not accepted.",
        )
    })?;
    let policy:DelegationPolicy=read_json(&state.join("policies").join(format!("sync-{id}.json"))).map_err(|_|DomainError::new("sync_delegation_required","No matching user-enabled local sync policy exists. Ask the user to activate a bounded local policy through the explicit CLI configuration procedure."))?;
    if policy.policy_id != id {
        return Err(git::io_error("policy identity"));
    }
    Ok(policy)
}
fn check_bindings(store: &Store, local: &Path, policy: &DelegationPolicy) -> Result<()> {
    agentlaw_storage::validate_id(&policy.policy_id).map_err(storage_error)?;
    let source = store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .map_err(storage_error)?;
    if policy.version == 0
        || source != policy.source
        || fs::canonicalize(local).map_err(|_| git::io_error("policy local binding"))?
            != policy.control
    {
        return Err(DomainError::new(
            "sync_policy_binding",
            "Delegation belongs to a different source/control binding.",
        ));
    }
    let fetch = git::text(&source, &["remote", "get-url", "--all", &policy.remote])?;
    if fetch != policy.fetch_endpoint
        || git::endpoint(&source, &policy.remote)? != policy.push_endpoint
        || !policy.target_ref.starts_with("refs/heads/")
    {
        return Err(DomainError::new("sync_destination_changed","Actual Git fetch/push endpoints differ from the registered policy. No new destination was authorized."));
    }
    git::run(&source, &["check-ref-format", &policy.target_ref])?;
    if policy
        .allowed_operations
        .iter()
        .any(|s| !matches!(s.as_str(), "memory" | "procedure" | "redirect" | "catalog"))
        || policy.allowed_scopes.iter().any(|s| {
            !matches!(
                s.as_str(),
                "user" | "machine" | "project" | "project_machine"
            )
        })
    {
        return Err(DomainError::new(
            "invalid_policy_scope",
            "Unknown delegation operation/applicability scope.",
        ));
    }
    Ok(())
}
pub(super) fn check(store: &Store, local: &Path, policy: &DelegationPolicy) -> Result<()> {
    check_bindings(store, local, policy)?;
    if !policy.enabled {
        return Err(DomainError::new(
            "sync_delegation_disabled",
            "The local sync policy is disabled; no undecided effects or push are authorized.",
        ));
    }
    Ok(())
}
pub(super) fn recheck(
    state: &Path,
    store: &Store,
    local: &Path,
    policy: &DelegationPolicy,
    expected: &str,
) -> Result<()> {
    let actual = load(state, &policy.policy_id)?;
    check(store, local, &actual)?;
    if digest(&actual) != expected {
        return Err(DomainError::new("sync_policy_changed","The delegated policy changed. Already-decided canonical redo remains mandatory; unstarted effects/push are blocked."));
    }
    Ok(())
}
pub(super) fn findings_accepted(op: &Operation) -> Result<bool> {
    let path = op.owned.join("findings-acceptance.json");
    if !path.exists() {
        return Ok(false);
    }
    let accepted: Value = read_json(&path)?;
    let scan = op.scan.as_ref().unwrap();
    Ok(accepted["candidate"] == scan.commit_oid
        && accepted["findings_digest"] == scan.findings_digest
        && accepted["scanner_digest"] == scan.scanner_build_digest
        && accepted["policy_digest"] == op.policy_digest
        && accepted["push_endpoint_digest"] == git::hash(op.policy.push_endpoint.as_bytes())
        && accepted["target_ref"] == op.policy.target_ref
        && accepted["authority_kind"] == "explicit_local_cli")
}
pub fn accept_findings(
    store: &Store,
    local: &Path,
    state: &Path,
    operation_id: &str,
    candidate: &str,
    findings_digest: &str,
    confirmed: bool,
) -> Result<Value> {
    if !confirmed {
        return Err(DomainError::new(
            "sharing_choice_required",
            "User approval of these exact sensitive findings is required.",
        ));
    }
    let _lane = git::lane(&local.join("git"))?;
    let conn = db(local)?;
    let raw: String = conn
        .query_row(
            "SELECT state FROM sync_operations WHERE operation_id=?1",
            [operation_id],
            |r| r.get(0),
        )
        .map_err(|_| git::io_error("findings operation"))?;
    let op: Operation =
        serde_json::from_str(&raw).map_err(|_| git::io_error("findings operation decode"))?;
    recheck(state, store, local, &op.policy, &op.policy_digest)?;
    let scan = op
        .scan
        .as_ref()
        .ok_or_else(|| git::io_error("findings scan"))?;
    if scan.commit_oid != candidate || scan.findings_digest != findings_digest {
        return Err(DomainError::new(
            "findings_approval_stale",
            "Candidate/findings differ from this exact explicit approval.",
        ));
    }
    let accepted = json!({"candidate":candidate,"findings_digest":findings_digest,"scanner_digest":scan.scanner_build_digest,"policy_digest":op.policy_digest,"push_endpoint_digest":git::hash(op.policy.push_endpoint.as_bytes()),"target_ref":op.policy.target_ref,"authority_kind":"explicit_local_cli"});
    git::save_local_json(&op.owned.join("findings-acceptance.json"), &accepted)?;
    Ok(accepted)
}
