//! Candidate-owned raw Git transfer store. Hash/structure/scan/materialization share
//! one batch stream. A persisted receipt skips patterns on retransmission/restart.
use crate::git_ops::{
    command, file_digest, hash, io_error, run, save_local_json, text, valid_oid, Finding, POLICY,
};
use agentlaw_contracts::{DomainError, Result};
use flate2::{write::ZlibEncoder, Compression};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
};

const SCANNER: &str = "agentlaw-raw-batch-patterns-v1";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Object {
    pub oid: String,
    pub kind: String,
    pub bytes: u64,
    pub compressed_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanReceipt {
    pub schema_version: u32,
    pub commit_oid: String,
    pub tree_oid: String,
    pub object_format: String,
    pub object_set_digest: String,
    pub object_count: u64,
    pub blob_count: u64,
    pub raw_bytes: u64,
    pub policy_version: String,
    pub policy_digest: String,
    pub scanner_id: String,
    pub scanner_build_digest: String,
    pub transfer_store: PathBuf,
    pub manifest_digest: String,
    pub payload_digest: String,
    pub findings: Vec<Finding>,
    pub findings_digest: String,
    pub completed_at_ms: u128,
    pub scope: String,
    pub profile: String,
}
fn unsupported() -> DomainError {
    DomainError::new("unsupported_git_object_profile","Sharing requires complete raw objects without replace refs, grafts, shallow/partial/promisor stores or alternates. No Git configuration was changed.")
}
pub fn check_profile(repo: &Path) -> Result<String> {
    for name in [
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_REPLACE_REF_BASE",
        "GIT_SHALLOW_FILE",
    ] {
        if std::env::var_os(name).is_some() {
            return Err(unsupported());
        }
    }
    let common = PathBuf::from(text(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    for name in ["info/grafts", "shallow", "objects/info/alternates"] {
        if common.join(name).exists() {
            return Err(unsupported());
        }
    }
    if !text(
        repo,
        &["for-each-ref", "--format=%(refname)", "refs/replace/"],
    )?
    .is_empty()
    {
        return Err(unsupported());
    }
    let partial = command(repo)
        .args([
            "config",
            "--get-regexp",
            "^(remote\\..*\\.promisor|extensions\\.partialClone)$",
        ])
        .output()
        .map_err(|_| io_error("object profile"))?;
    if partial.status.success() && !partial.stdout.is_empty() {
        return Err(unsupported());
    }
    if !partial.status.success() && partial.status.code() != Some(1) {
        return Err(io_error("object profile configuration"));
    }
    let format = text(repo, &["rev-parse", "--show-object-format"])?;
    if !matches!(format.as_str(), "sha1" | "sha256") {
        return Err(unsupported());
    }
    Ok(format)
}
struct HashedFile {
    file: File,
    hash: Sha256,
}
impl Write for HashedFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(bytes)?;
        self.hash.update(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
enum ObjectHash {
    Sha1(sha1::Sha1),
    Sha256(Sha256),
}
impl ObjectHash {
    fn new(format: &str) -> Self {
        if format == "sha1" {
            Self::Sha1(sha1::Sha1::new())
        } else {
            Self::Sha256(Sha256::new())
        }
    }
    fn update(&mut self, b: &[u8]) {
        match self {
            Self::Sha1(h) => h.update(b),
            Self::Sha256(h) => h.update(b),
        }
    }
    fn finish(self) -> String {
        match self {
            Self::Sha1(h) => format!("{:x}", h.finalize()),
            Self::Sha256(h) => format!("{:x}", h.finalize()),
        }
    }
}
const PATTERNS: [(&[u8], &str); 6] = [
    (b"-----BEGIN PRIVATE KEY-----", "private_key"),
    (b"-----BEGIN RSA PRIVATE KEY-----", "private_key"),
    (b"-----BEGIN OPENSSH PRIVATE KEY-----", "private_key"),
    (b"ghp_", "github_token_prefix"),
    (b"github_pat_", "github_token_prefix"),
    (b"sk-proj-", "openai_project_token_prefix"),
];
fn scanner_digest() -> String {
    hash(include_bytes!("git_scan.rs"))
}
fn policy_digest() -> String {
    let mut bytes = POLICY.as_bytes().to_vec();
    for (p, c) in PATTERNS {
        bytes.extend(p);
        bytes.extend(c.as_bytes());
    }
    hash(&bytes)
}

pub fn seal(repo: &Path, commit: &str, owned: &Path) -> Result<ScanReceipt> {
    let receipt_path = owned.join("scan.json");
    if receipt_path.exists() {
        let receipt: ScanReceipt = serde_json::from_reader(
            File::open(&receipt_path).map_err(|_| io_error("scan receipt"))?,
        )
        .map_err(|_| io_error("scan receipt decode"))?;
        if receipt.commit_oid != commit {
            return Err(DomainError::new(
                "scan_binding_mismatch",
                "Sealed receipt belongs to another candidate.",
            ));
        }
        validate(&receipt)?;
        return Ok(receipt);
    }
    let format = check_profile(repo)?;
    if !valid_oid(commit) {
        return Err(io_error("candidate OID"));
    }
    fs::create_dir_all(owned).map_err(|_| io_error("scan directory"))?;
    // A failed pre-receipt attempt is retained; a new owned store avoids treating
    // partially materialized objects as a completed scan.
    let transfer = owned.join(format!("objects-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&transfer).map_err(|_| io_error("transfer directory"))?;
    run(
        &transfer,
        &["init", "--bare", &format!("--object-format={format}")],
    )?;
    let mut inventory = command(repo)
        .args(["rev-list", "--objects", "--no-object-names", commit])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| io_error("object inventory process"))?;
    let mut oids = BTreeSet::new();
    let enumerated = (|| -> Result<()> {
        for line in BufReader::new(
            inventory
                .stdout
                .take()
                .ok_or_else(|| io_error("object inventory pipe"))?,
        )
        .lines()
        {
            let id = line.map_err(|_| io_error("object inventory line"))?;
            if !valid_oid(&id) || id.len() != commit.len() {
                return Err(io_error("object inventory identity"));
            }
            oids.insert(id);
            if oids.len() > 1_000_000 {
                return Err(DomainError::new(
                    "git_object_inventory_capacity",
                    "The bounded transfer profile allows at most one million reachable objects.",
                ));
            }
        }
        Ok(())
    })();
    if enumerated.is_err() {
        let _ = inventory.kill();
    }
    let inventory_status = inventory
        .wait()
        .map_err(|_| io_error("object inventory wait"))?;
    enumerated?;
    if !inventory_status.success() {
        return Err(io_error("object inventory completion"));
    }
    let mut child = command(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| io_error("batch object stream"))?;
    let mut stdin = child.stdin.take().ok_or_else(|| io_error("batch input"))?;
    let mut reader = BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| io_error("batch output"))?,
    );
    let mut objects = Vec::new();
    let mut references = Vec::new();
    let mut findings = BTreeSet::new();
    let result = std::thread::scope(|scope| -> Result<()> {
        let writer = scope.spawn(|| -> std::io::Result<()> {
            for oid in &oids {
                writeln!(stdin, "{oid}")?;
            }
            drop(stdin);
            Ok(())
        });
        let read_result = (|| -> Result<()> {
            for expected in &oids {
                let mut header = String::new();
                reader
                    .read_line(&mut header)
                    .map_err(|_| io_error("batch header"))?;
                let parts = header.split_whitespace().collect::<Vec<_>>();
                if parts.len() != 3
                    || parts[0] != expected
                    || !matches!(parts[1], "commit" | "tree" | "blob")
                {
                    return Err(io_error("raw object header/type"));
                }
                let bytes = parts[2]
                    .parse::<u64>()
                    .map_err(|_| io_error("object size"))?;
                let mut metadata = Vec::new();
                let path = transfer
                    .join("objects")
                    .join(&expected[..2])
                    .join(&expected[2..]);
                fs::create_dir_all(path.parent().unwrap())
                    .map_err(|_| io_error("loose object directory"))?;
                let temp = path.with_extension("partial");
                let output = HashedFile {
                    file: File::create(&temp).map_err(|_| io_error("loose object create"))?,
                    hash: Sha256::new(),
                };
                let mut encoder = ZlibEncoder::new(output, Compression::default());
                let raw_header = format!("{} {}\0", parts[1], bytes);
                encoder
                    .write_all(raw_header.as_bytes())
                    .map_err(|_| io_error("object header materialization"))?;
                let mut object_hash = ObjectHash::new(&format);
                object_hash.update(raw_header.as_bytes());
                let mut remaining = bytes;
                let mut buffer = [0u8; 65536];
                let mut carry = Vec::new();
                while remaining > 0 {
                    let n = (remaining.min(buffer.len() as u64)) as usize;
                    reader
                        .read_exact(&mut buffer[..n])
                        .map_err(|_| io_error("raw object body"))?;
                    remaining -= n as u64;
                    object_hash.update(&buffer[..n]);
                    encoder
                        .write_all(&buffer[..n])
                        .map_err(|_| io_error("object materialization"))?;
                    if parts[1] == "blob" {
                        carry.extend_from_slice(&buffer[..n]);
                        for (pattern, category) in PATTERNS {
                            if carry.windows(pattern.len()).any(|w| w == pattern) {
                                findings.insert(Finding {
                                    category: category.into(),
                                    blob_oid: expected.clone(),
                                });
                            }
                        }
                        let keep = carry.len().saturating_sub(64);
                        carry.drain(..keep);
                    } else {
                        if metadata.len() + n > 16 * 1024 * 1024 {
                            return Err(DomainError::new("unsupported_git_metadata_size","A commit/tree exceeds the bounded raw metadata validation profile."));
                        }
                        metadata.extend_from_slice(&buffer[..n]);
                    }
                }
                let mut newline = [0u8; 1];
                reader
                    .read_exact(&mut newline)
                    .map_err(|_| io_error("batch object delimiter"))?;
                if newline != [b'\n'] || object_hash.finish() != *expected {
                    return Err(DomainError::new(
                        "git_object_integrity",
                        "Raw Git object hash/delimiter mismatch; no sharing occurred.",
                    ));
                }
                references.extend(structural_refs(
                    expected,
                    parts[1],
                    &metadata,
                    commit.len() / 2,
                )?);
                let output = encoder
                    .finish()
                    .map_err(|_| io_error("object compression finish"))?;
                output
                    .file
                    .sync_all()
                    .map_err(|_| io_error("object sync"))?;
                let digest = format!("{:x}", output.hash.finalize());
                drop(output.file);
                fs::rename(temp, path).map_err(|_| io_error("object seal"))?;
                objects.push(Object {
                    oid: expected.clone(),
                    kind: parts[1].into(),
                    bytes,
                    compressed_sha256: digest,
                });
            }
            Ok(())
        })();
        if read_result.is_err() {
            let _ = child.kill();
        }
        let written = writer.join().map_err(|_| io_error("batch writer join"))?;
        read_result?;
        written.map_err(|_| io_error("batch writer"))?;
        Ok(())
    });
    let status = child.wait().map_err(|_| io_error("batch object wait"))?;
    result?;
    if !status.success() {
        return Err(io_error("batch object completion"));
    }
    let types = objects
        .iter()
        .map(|o| (o.oid.as_str(), o.kind.as_str()))
        .collect::<BTreeMap<_, _>>();
    for (owner, target, kind) in references {
        if types.get(target.as_str()).copied() != Some(kind.as_str()) {
            return Err(DomainError::new(
                "git_object_connectivity",
                format!("Sealed {owner} references a missing or wrong-type object."),
            ));
        }
    }
    if types.get(commit).copied() != Some("commit") {
        return Err(io_error("candidate commit type"));
    }
    let tree = text(repo, &["rev-parse", &format!("{commit}^{{tree}}")])?;
    if types.get(tree.as_str()).copied() != Some("tree") {
        return Err(io_error("candidate tree type"));
    }
    let manifest = serde_json::to_vec(&objects).map_err(|_| io_error("object manifest"))?;
    save_local_json(&owned.join("objects.json"), &objects)?;
    let findings = findings.into_iter().collect::<Vec<_>>();
    let set = objects
        .iter()
        .map(|o| (&o.oid, &o.kind, o.bytes))
        .collect::<Vec<_>>();
    let payload = objects
        .iter()
        .map(|o| (&o.oid, &o.compressed_sha256))
        .collect::<Vec<_>>();
    run(
        &transfer,
        &["update-ref", "refs/agentlaw/candidate", commit],
    )?;
    let receipt = ScanReceipt {
        schema_version: 1,
        commit_oid: commit.into(),
        tree_oid: tree,
        object_format: format,
        object_set_digest: hash(&serde_json::to_vec(&set).unwrap()),
        object_count: objects.len() as u64,
        blob_count: objects.iter().filter(|o| o.kind == "blob").count() as u64,
        raw_bytes: objects.iter().map(|o| o.bytes).sum(),
        policy_version: POLICY.into(),
        policy_digest: policy_digest(),
        scanner_id: SCANNER.into(),
        scanner_build_digest: scanner_digest(),
        transfer_store: fs::canonicalize(transfer)
            .map_err(|_| io_error("sealed store identity"))?,
        manifest_digest: hash(&manifest),
        payload_digest: hash(&serde_json::to_vec(&payload).unwrap()),
        findings_digest: hash(&serde_json::to_vec(&findings).unwrap()),
        findings,
        completed_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        scope: "all_reachable_raw_blobs".into(),
        profile: "complete_self_contained_raw_v1".into(),
    };
    save_local_json(&receipt_path, &receipt)?;
    Ok(receipt)
}
fn structural_refs(
    owner: &str,
    kind: &str,
    body: &[u8],
    oid_bytes: usize,
) -> Result<Vec<(String, String, String)>> {
    let mut result = Vec::new();
    if kind == "commit" {
        let mut tree = 0;
        for line in body.split(|b| *b == b'\n') {
            if line.is_empty() {
                break;
            }
            let expected = if line.starts_with(b"tree ") {
                tree += 1;
                Some("tree")
            } else if line.starts_with(b"parent ") {
                Some("commit")
            } else {
                None
            };
            if let Some(kind) = expected {
                let oid =
                    std::str::from_utf8(&line[line.iter().position(|b| *b == b' ').unwrap() + 1..])
                        .map_err(|_| io_error("commit reference"))?;
                if !valid_oid(oid) || oid.len() != oid_bytes * 2 {
                    return Err(io_error("commit reference OID"));
                }
                result.push((owner.into(), oid.into(), kind.into()));
            }
        }
        if tree != 1 {
            return Err(io_error("commit tree cardinality"));
        }
    } else if kind == "tree" {
        let mut offset = 0;
        let mut names = BTreeSet::new();
        while offset < body.len() {
            let space = body[offset..]
                .iter()
                .position(|b| *b == b' ')
                .ok_or_else(|| io_error("tree mode"))?
                + offset;
            let nul = body[space + 1..]
                .iter()
                .position(|b| *b == 0)
                .ok_or_else(|| io_error("tree name"))?
                + space
                + 1;
            let name = &body[space + 1..nul];
            if name.is_empty()
                || name == b"."
                || name == b".."
                || name.contains(&b'/')
                || !names.insert(name.to_vec())
            {
                return Err(io_error("tree entry name"));
            }
            let kind =
                match &body[offset..space] {
                    b"40000" | b"040000" => "tree",
                    b"100644" | b"100755" | b"120000" => "blob",
                    b"160000" => return Err(DomainError::new(
                        "unsupported_gitlink",
                        "Gitlink/submodule trees are outside the self-contained transfer profile.",
                    )),
                    _ => return Err(io_error("tree entry mode")),
                };
            let end = nul + 1 + oid_bytes;
            if end > body.len() {
                return Err(io_error("tree object boundary"));
            }
            let oid = body[nul + 1..end]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            result.push((owner.into(), oid, kind.into()));
            offset = end;
        }
    }
    Ok(result)
}
pub fn validate(receipt: &ScanReceipt) -> Result<()> {
    if receipt.schema_version != 1
        || receipt.policy_version != POLICY
        || receipt.policy_digest != policy_digest()
        || receipt.scanner_id != SCANNER
        || receipt.scanner_build_digest != scanner_digest()
    {
        return Err(DomainError::new(
            "scan_receipt_stale",
            "Scan policy/build changed; create a new candidate scan receipt.",
        ));
    }
    let owned = receipt
        .transfer_store
        .parent()
        .ok_or_else(|| io_error("sealed directory"))?;
    let objects: Vec<Object> = serde_json::from_reader(
        File::open(owned.join("objects.json")).map_err(|_| io_error("sealed manifest read"))?,
    )
    .map_err(|_| io_error("sealed manifest decode"))?;
    if hash(&serde_json::to_vec(&objects).unwrap()) != receipt.manifest_digest
        || objects.len() as u64 != receipt.object_count
        || hash(&serde_json::to_vec(&receipt.findings).unwrap()) != receipt.findings_digest
    {
        return Err(DomainError::new(
            "scan_receipt_integrity",
            "Sealed manifest/findings changed; sharing is blocked.",
        ));
    }
    let mut payload = Vec::new();
    let mut seen = BTreeSet::new();
    for object in &objects {
        if !valid_oid(&object.oid)
            || object.oid.len() != receipt.commit_oid.len()
            || !seen.insert(&object.oid)
        {
            return Err(io_error("sealed manifest object identity"));
        }
        let path = receipt
            .transfer_store
            .join("objects")
            .join(&object.oid[..2])
            .join(&object.oid[2..]);
        if file_digest(&path)?.as_deref() != Some(&object.compressed_sha256) {
            return Err(DomainError::new("sealed_payload_integrity","Sealed transfer data is missing or changed; no patterns were rescanned and no sharing occurred."));
        }
        payload.push((&object.oid, &object.compressed_sha256));
    }
    let set = objects
        .iter()
        .map(|o| (&o.oid, &o.kind, o.bytes))
        .collect::<Vec<_>>();
    if hash(&serde_json::to_vec(&payload).unwrap()) != receipt.payload_digest
        || hash(&serde_json::to_vec(&set).unwrap()) != receipt.object_set_digest
        || check_profile(&receipt.transfer_store)? != receipt.object_format
    {
        return Err(io_error("sealed receipt binding"));
    }
    if text(
        &receipt.transfer_store,
        &["rev-parse", "refs/agentlaw/candidate"],
    )? != receipt.commit_oid
    {
        return Err(io_error("sealed candidate ref"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn repository(format: &str) -> (tempfile::TempDir, PathBuf, String) {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("source with spaces");
        fs::create_dir(&repo).unwrap();
        run(&repo, &["init", &format!("--object-format={format}")]).unwrap();
        run(&repo, &["config", "user.name", "Scan Test"]).unwrap();
        run(&repo, &["config", "user.email", "scan@example.invalid"]).unwrap();
        fs::write(
            repo.join("old-secret.txt"),
            b"deleted historical ghp_test_prefix",
        )
        .unwrap();
        run(&repo, &["add", "."]).unwrap();
        run(&repo, &["commit", "-m", "historical secret"]).unwrap();
        run(&repo, &["rm", "old-secret.txt"]).unwrap();
        fs::write(repo.join("now.txt"), b"clean current text").unwrap();
        run(&repo, &["add", "."]).unwrap();
        run(&repo, &["commit", "-m", "current without secret"]).unwrap();
        let oid = text(&repo, &["rev-parse", "HEAD"]).unwrap();
        (temp, repo, oid)
    }
    #[test]
    fn history_scan_receipt_and_independent_transfer() {
        let (t, repo, oid) = repository("sha1");
        let receipt = seal(&repo, &oid, &t.path().join("seal")).unwrap();
        assert!(receipt
            .findings
            .iter()
            .any(|f| f.category == "github_token_prefix"));
        assert_eq!(receipt.blob_count, 2);
        validate(&receipt).unwrap();
        let again = seal(&repo, &oid, &t.path().join("seal")).unwrap();
        assert_eq!(receipt.completed_at_ms, again.completed_at_ms);
        fs::rename(
            repo.join(".git/objects"),
            repo.join(".git/objects-retained"),
        )
        .unwrap();
        assert_eq!(
            text(
                &receipt.transfer_store,
                &["rev-parse", "refs/agentlaw/candidate"]
            )
            .unwrap(),
            oid
        );
        assert!(text(
            &receipt.transfer_store,
            &["show", &format!("{oid}:now.txt")]
        )
        .unwrap()
        .contains("clean"));
    }
    #[test]
    fn tampered_sealed_data_blocks_without_rescan() {
        let (t, repo, oid) = repository("sha1");
        let receipt = seal(&repo, &oid, &t.path().join("seal")).unwrap();
        let path = receipt
            .transfer_store
            .join("objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        fs::write(path, b"not the sealed object").unwrap();
        let error = validate(&receipt).unwrap_err();
        assert_eq!(error.code, "sealed_payload_integrity");
        let stored: ScanReceipt =
            serde_json::from_reader(File::open(t.path().join("seal/scan.json")).unwrap()).unwrap();
        assert_eq!(stored.completed_at_ms, receipt.completed_at_ms);
    }
    #[test]
    fn unsupported_alternate_is_not_reconfigured() {
        let (t, repo, _) = repository("sha1");
        let path = repo.join(".git/objects/info/alternates");
        fs::write(&path, t.path().to_string_lossy().as_bytes()).unwrap();
        let error = check_profile(&repo).unwrap_err();
        assert_eq!(error.code, "unsupported_git_object_profile");
        assert!(path.exists());
    }
    #[test]
    fn sha256_raw_objects_are_verified() {
        let (t, repo, oid) = repository("sha256");
        let receipt = seal(&repo, &oid, &t.path().join("seal")).unwrap();
        assert_eq!(receipt.object_format, "sha256");
        assert_eq!(oid.len(), 64);
        validate(&receipt).unwrap();
    }
}
