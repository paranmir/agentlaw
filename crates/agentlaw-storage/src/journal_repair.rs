//! Explicit C6 journal replacement from bound durable receipts. Originals and
//! pending recovery material are retained; a damaged proposal DB is not guessed.
use super::*;
#[derive(Debug, Serialize, Deserialize)]
pub struct JournalRepairReport {
    pub backup_directory: PathBuf,
    pub reconstructed_publications: u64,
    pub source_generation: u64,
    pub canonical_audit: history_spool::SourceAuditReport,
    pub pending_material_preserved: bool,
}
#[derive(Serialize, Deserialize)]
struct RepairPlan {
    repair_id: String,
    candidate_digest: String,
    originals: BTreeMap<String, String>,
}
impl Store {
    pub fn repair_local_journal(
        root: impl AsRef<Path>,
        local: impl AsRef<Path>,
    ) -> Result<JournalRepairReport> {
        let coordination = local_coordination(local.as_ref());
        Self::repair_local_journal_with_coordination(root, local, coordination)
    }
    pub fn repair_local_journal_with_coordination(
        root: impl AsRef<Path>,
        local: impl AsRef<Path>,
        coordination: impl AsRef<Path>,
    ) -> Result<JournalRepairReport> {
        let store = Store {
            root: fs::canonicalize(root)?,
            local: fs::canonicalize(local)?,
            coordination: coordination.as_ref().to_path_buf(),
        };
        store.check_local_binding(false)?;
        let admission = store.admission_lock()?;
        let gate = store.lock()?;
        store.validate_format()?;
        store.resume_journal_repair_locked()?;
        let fence = store.fence()?;
        let generation = fence.generation;
        let repair_id = uuid::Uuid::new_v4().to_string();
        let dir = store.local.join("journal-repairs").join(&repair_id);
        fs::create_dir_all(&dir)?;
        let candidate = dir.join("candidate.sqlite");
        let mut conn = journal::open(&candidate)?;
        let tx = conn.transaction()?;
        let recovery = store.local.join("recovery");
        if recovery.exists() {
            for entry in fs::read_dir(&recovery)? {
                let path = entry?.path();
                if !path.is_dir() || !path.join("published").exists() {
                    continue;
                }
                let manifest: Manifest = store.load(&path.join("manifest"))?;
                store.validate_completed_material(&path, &manifest)?;
                let allowed = if fence.operation_id.as_deref() == Some(&manifest.operation_id) {
                    generation.checked_add(1).ok_or(Error::Capacity)?
                } else {
                    generation
                };
                if manifest.receipt.generation > allowed {
                    return Err(Error::RecoveryRequired("retained receipt is beyond the source fence; inspect original binding before repair".into()));
                }
                tx.execute("INSERT INTO publications(operation_id,receipt,manifest,sequence) VALUES(?1,?2,?3,?4)",rusqlite::params![manifest.operation_id,serde_json::to_string(&manifest.receipt)?,serde_json::to_string(&manifest)?,manifest.receipt.generation.to_be_bytes().as_slice()]).map_err(|_|Error::RecoveryRequired("durable receipts conflict; repair candidate retained without replacing journal".into()))?;
            }
        }
        let count = {
            let mut query = tx.prepare("SELECT sequence FROM publications ORDER BY sequence")?;
            let mut rows = query.query([])?;
            let mut expected = 0u64;
            while let Some(row) = rows.next()? {
                expected = expected.checked_add(1).ok_or(Error::Capacity)?;
                let sequence: Vec<u8> = row.get(0)?;
                if sequence != expected.to_be_bytes() {
                    return Err(Error::RecoveryRequired("retained publication coverage has gaps; restore missing recovery decisions or backup, original journal untouched".into()));
                }
            }
            expected
        };
        if count < generation {
            return Err(Error::RecoveryRequired("not all published generations have retained authoritative receipts; original journal untouched".into()));
        }
        tx.commit()?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(Error::RecoveryRequired(
                "reconstructed journal failed integrity check".into(),
            ));
        }
        drop(conn);
        let mut originals = BTreeMap::new();
        for name in ["journal.sqlite", "journal.sqlite-wal", "journal.sqlite-shm"] {
            let path = store.local.join(name);
            if !path.exists() {
                continue;
            }
            if fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(Error::RecoveryRequired(
                    "local journal path is a symlink; inspect binding before repair".into(),
                ));
            }
            let digest = hash_file(&path)?.unwrap();
            let backup = dir.join(format!("original-{name}"));
            install_reader(&backup, &mut File::open(&path)?)?;
            if hash_file(&backup)?.as_deref() != Some(&digest)
                || hash_file(&path)?.as_deref() != Some(&digest)
            {
                return Err(Error::RecoveryRequired(
                    "journal changed during backup; close other local database clients and retry"
                        .into(),
                ));
            }
            originals.insert(name.into(), digest);
        }
        let plan = RepairPlan {
            repair_id,
            candidate_digest: hash_file(&candidate)?.unwrap(),
            originals,
        };
        store.record(&dir.join("plan"), &plan)?;
        store.record(&store.local.join("journal-repair"), &plan)?;
        store.resume_journal_repair_locked()?;
        store.recover_locked()?;
        let final_generation = store.ensure_clean()?;
        drop(gate);
        drop(admission);
        let canonical_audit = store.audit_source()?;
        if canonical_audit.source_position.sequence != final_generation {
            return Err(Error::Stale(
                "source changed after journal recovery; retry audit".into(),
            ));
        }
        {
            let _gate = store.lock()?;
            journal::reject_existing_changes(&store, &BTreeSet::new())?;
        }
        Ok(JournalRepairReport {
            backup_directory: dir,
            reconstructed_publications: count,
            source_generation: final_generation,
            canonical_audit,
            pending_material_preserved: true,
        })
    }
    pub(super) fn resume_journal_repair_locked(&self) -> Result<()> {
        let marker = self.local.join("journal-repair");
        if !marker.exists() {
            return Ok(());
        }
        let plan: RepairPlan = self.load(&marker)?;
        validate_id(&plan.repair_id)?;
        let dir = self.local.join("journal-repairs").join(&plan.repair_id);
        let candidate = dir.join("candidate.sqlite");
        if hash_file(&candidate)?.as_deref() != Some(&plan.candidate_digest) {
            return Err(Error::RecoveryRequired("journal repair candidate missing/changed; original backups are retained under journal-repairs".into()));
        }
        for name in ["journal.sqlite-wal", "journal.sqlite-shm", "journal.sqlite"] {
            let path = self.local.join(name);
            let retired = dir.join(format!("retired-{name}"));
            if let Some(expected) = plan.originals.get(name) {
                if retired.exists() {
                    if hash_file(&retired)?.as_deref() != Some(expected) {
                        return Err(Error::RecoveryRequired(
                            "retired journal backup changed".into(),
                        ));
                    }
                    continue;
                }
                if hash_file(&path)?.as_deref() != Some(expected) {
                    return Err(Error::RecoveryRequired("original journal differs from backed-up repair basis; no files overwritten".into()));
                }
                fs::rename(&path, &retired)?;
            } else if path.exists()
                && !(name == "journal.sqlite"
                    && hash_file(&path)?.as_deref() == Some(&plan.candidate_digest))
            {
                return Err(Error::RecoveryRequired(
                    "unplanned journal sidecar appeared during repair".into(),
                ));
            }
        }
        if hash_file(&self.local.join("journal.sqlite"))?.as_deref() != Some(&plan.candidate_digest)
        {
            install_reader(
                &self.local.join("journal.sqlite"),
                &mut File::open(candidate)?,
            )?;
        }
        fs::remove_file(marker)?;
        use persistence::Persistence;
        persistence::PlatformPersistence.sync_namespace(&self.local)?;
        Ok(())
    }
}
