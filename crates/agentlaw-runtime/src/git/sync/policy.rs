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
/// An explicitly confirmed policy whose existing shape and static domain
/// constraints were checked.
/// Source/control and actual Git destination checks still require a store.
#[derive(Debug)]
pub struct PreparedPolicy(DelegationPolicy);

pub fn require_delegation_confirmation(confirmed: bool) -> Result<()> {
    if !confirmed {
        return Err(DomainError::new("delegation_confirmation_required","The user must explicitly activate this continuing local delegation. It is not per-solution approval."));
    }
    Ok(())
}

pub fn require_sharing_confirmation(confirmed: bool) -> Result<()> {
    if !confirmed {
        return Err(DomainError::new(
            "sharing_choice_required",
            "User approval of these exact sensitive findings is required.",
        ));
    }
    Ok(())
}

pub fn prepare_policy_configuration(input: &str, confirmed: bool) -> Result<PreparedPolicy> {
    require_delegation_confirmation(confirmed)?;
    let value = agentlaw_contracts::validation::decode_unique(input)
        .map_err(|_| git::io_error("sync record decode"))?;
    let policy: DelegationPolicy =
        serde_json::from_value(value).map_err(|_| git::io_error("sync record decode"))?;
    validate_policy(&policy)?;
    Ok(PreparedPolicy(policy))
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
    require_delegation_confirmation(confirmed)?;
    let policy: DelegationPolicy = read_json(file)?;
    validate_policy(&policy)?;
    configure_prepared_policy(state, store, local, PreparedPolicy(policy))
}

pub fn configure_prepared_policy(
    state: &Path,
    store: &Store,
    local: &Path,
    prepared: PreparedPolicy,
) -> Result<Value> {
    let policy = prepared.0;
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
fn validate_policy(policy: &DelegationPolicy) -> Result<()> {
    agentlaw_storage::validate_id(&policy.policy_id).map_err(storage_error)?;
    if policy.version == 0 {
        return Err(DomainError::new(
            "sync_policy_binding",
            "Delegation belongs to a different source/control binding.",
        ));
    }
    if !policy.target_ref.starts_with("refs/heads/") {
        return Err(DomainError::new("sync_destination_changed","Actual Git fetch/push endpoints differ from the registered policy. No new destination was authorized."));
    }
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
fn check_bindings(store: &Store, local: &Path, policy: &DelegationPolicy) -> Result<()> {
    let source = store
        .with_source_read(|p, _| Ok(p.to_path_buf()))
        .map_err(storage_error)?;
    if source != policy.source
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
    {
        return Err(DomainError::new("sync_destination_changed","Actual Git fetch/push endpoints differ from the registered policy. No new destination was authorized."));
    }
    git::run(&source, &["check-ref-format", &policy.target_ref])?;
    Ok(())
}
pub(super) fn check(store: &Store, local: &Path, policy: &DelegationPolicy) -> Result<()> {
    validate_policy(policy)?;
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
    require_sharing_confirmation(confirmed)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Value {
        json!({
            "policy_id":"c62664c6-0988-43f5-878c-d2d14f59d210",
            "version":1,
            "enabled":true,
            "source":"unavailable-source",
            "control":"unavailable-control",
            "remote":"origin",
            "fetch_endpoint":"unavailable-remote",
            "push_endpoint":"unavailable-remote",
            "target_ref":"refs/heads/main",
            "allowed_operations":["memory"],
            "allowed_scopes":["user"],
            "push_permission":true
        })
    }

    #[test]
    fn prepared_policy_requires_confirmation_before_decoding() {
        assert_eq!(
            prepare_policy_configuration("not JSON", false)
                .unwrap_err()
                .code,
            "delegation_confirmation_required"
        );
    }

    #[test]
    fn prepares_existing_static_policy_contract_without_source_access() {
        let prepared = prepare_policy_configuration(&policy().to_string(), true).unwrap();
        assert!(prepared.0.enabled);
        assert_eq!(prepared.0.version, 1);
    }

    #[test]
    fn prepared_policy_reuses_existing_constraint_errors() {
        for (field, value, expected) in [
            ("version", json!(0), "sync_policy_binding"),
            (
                "target_ref",
                json!("refs/tags/main"),
                "sync_destination_changed",
            ),
            (
                "allowed_operations",
                json!(["unknown"]),
                "invalid_policy_scope",
            ),
            ("allowed_scopes", json!(["unknown"]), "invalid_policy_scope"),
        ] {
            let mut input = policy();
            input[field] = value;
            assert_eq!(
                prepare_policy_configuration(&input.to_string(), true)
                    .unwrap_err()
                    .code,
                expected
            );
        }
    }

    #[test]
    fn prepared_policy_preserves_decode_error_and_rejects_duplicate_fields() {
        for input in [
            "{}".to_owned(),
            policy()
                .to_string()
                .replacen("\"version\":1", "\"version\":1,\"version\":2", 1),
        ] {
            assert_eq!(
                prepare_policy_configuration(&input, true).unwrap_err().code,
                "git_io"
            );
        }
    }
}
