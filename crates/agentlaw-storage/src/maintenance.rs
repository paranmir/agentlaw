//! Explicit local maintenance. Canonical files and idempotency receipts are never removed.
use super::*;
#[derive(Debug, Serialize, Deserialize)]
pub struct RecoveryGcReport {
    pub removed_images: u64,
    pub removed_bytes: u64,
    pub retained_through_generation: u64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct LedgerRebuildReport {
    pub publications: u64,
    pub generation: u64,
    pub existing_rows_preserved: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct MaintenanceReport {
    pub recovery: RecoveryGcReport,
    pub removed_history_caches: u64,
}
impl Store {
    /// Fixed conservative retention; never pending operations, source, or leased cache files.
    pub fn maintain_owned_material(&self) -> Result<MaintenanceReport> {
        let generation = self.generation()?;
        let recovery = self.gc_recovery_images(generation.saturating_sub(64))?;
        let _gate = self.lock()?;
        let generation = self.ensure_clean()?;
        let epoch: String = self.load(&self.local.join("source-epoch"))?;
        let root = self.local.join("history-cache");
        let mut removed = 0;
        if root.exists() {
            for entry in fs::read_dir(&root)? {
                let path = entry?.path();
                if path.extension().is_none_or(|s| s != "sqlite") {
                    continue;
                }
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| Error::Corrupt("cache filename".into()))?;
                let Some((cache_epoch, sequence)) = stem.rsplit_once('-') else {
                    continue;
                };
                if validate_id(cache_epoch).is_err() {
                    continue;
                }
                let Ok(sequence) = sequence.parse::<u64>() else {
                    continue;
                };
                if cache_epoch == epoch && sequence >= generation.saturating_sub(2) {
                    continue;
                }
                let lease = OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(path.with_extension("lease"))?;
                if lease.try_lock_exclusive().is_err() {
                    continue;
                }
                if fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(MaintenanceReport {
            recovery,
            removed_history_caches: removed,
        })
    }
    pub(super) fn automatic_maintenance(&self) {
        let Ok(generation) = self.generation() else {
            return;
        };
        if generation < 64 || generation % 64 != 0 {
            return;
        }
        let marker = self.local.join("maintenance-generation");
        if self.load::<u64>(&marker).ok() == Some(generation) {
            return;
        }
        match self.maintain_owned_material() {
            Ok(report) => {
                let _ = self.record(&self.local.join("maintenance-last-report"), &report);
                let _ = self.record(&marker, &generation);
            }
            Err(error) => {
                let _ = self.record(
                    &self.local.join("maintenance-last-error"),
                    &error.to_string(),
                );
            }
        }
    }
    /// Remove only completed after-images older than the caller's retention watermark.
    /// Manifest, decision and receipt remain permanently available for replay/ledger repair.
    pub fn gc_recovery_images(&self, retain_from_generation: u64) -> Result<RecoveryGcReport> {
        let _gate = self.lock()?;
        let generation = self.ensure_clean()?;
        if retain_from_generation > generation {
            return Err(Error::Corrupt(
                "GC retention watermark beyond source".into(),
            ));
        }
        let mut report = RecoveryGcReport {
            removed_images: 0,
            removed_bytes: 0,
            retained_through_generation: generation,
        };
        let root = self.local.join("recovery");
        if !root.exists() {
            return Ok(report);
        }
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            if !path.is_dir() || !path.join("published").exists() {
                continue;
            }
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path.join("execution.lock"))?;
            if lock.try_lock_exclusive().is_err() {
                continue;
            }
            let manifest: Manifest = self.load(&path.join("manifest"))?;
            if manifest.receipt.generation >= retain_from_generation {
                continue;
            }
            self.validate_completed_material(&path, &manifest)?;
            for target in &manifest.targets {
                let image = Path::new(&target.image);
                if image.components().count() != 1
                    || !matches!(
                        image.components().next(),
                        Some(std::path::Component::Normal(_))
                    )
                {
                    return Err(Error::Corrupt("unsafe recovery image".into()));
                }
                let image = path.join(image);
                if !image.exists() {
                    continue;
                }
                if hash_file(&image)?.as_deref() != Some(&target.digest) {
                    return Err(Error::Corrupt("recovery image digest before GC".into()));
                }
                report.removed_bytes = report
                    .removed_bytes
                    .checked_add(image.metadata()?.len())
                    .ok_or(Error::Capacity)?;
                fs::remove_file(image)?;
                report.removed_images += 1;
            }
            use persistence::Persistence;
            persistence::PlatformPersistence.sync_namespace(&path)?;
        }
        Ok(report)
    }
    pub(super) fn validate_completed_material(&self, path: &Path, m: &Manifest) -> Result<()> {
        if m.root != self.root.to_string_lossy() {
            return Err(Error::Corrupt("recovery root binding".into()));
        }
        let digest = codec::digest(&fs::read(path.join("manifest"))?);
        let decision: String = self.load(&path.join("decision"))?;
        let receipt: PublishReceipt = self.load(&path.join("published"))?;
        if decision != digest || serde_json::to_value(receipt)? != serde_json::to_value(&m.receipt)?
        {
            return Err(Error::Corrupt("completed recovery material binding".into()));
        }
        Ok(())
    }
    /// Reconstruct a missing/incomplete ledger from retained, bound published manifests.
    /// All generations must be present. Existing conflicting rows fail closed, never overwritten.
    pub fn rebuild_control_ledger(&self) -> Result<LedgerRebuildReport> {
        let audit = self.audit_source()?;
        let _gate = self.lock()?;
        let generation = self.ensure_clean()?;
        if generation != audit.source_position.sequence {
            return Err(Error::Stale("source changed during repair audit".into()));
        }
        let mut conn = journal::open(&self.local.join("journal.sqlite"))?;
        let tx = conn.transaction()?;
        let root = self.local.join("recovery");
        if root.exists() {
            for entry in fs::read_dir(root)? {
                let path = entry?.path();
                if !path.is_dir() || !path.join("published").exists() {
                    continue;
                }
                let m: Manifest = self.load(&path.join("manifest"))?;
                self.validate_completed_material(&path, &m)?;
                if m.receipt.generation > generation {
                    return Err(Error::Corrupt("receipt beyond clean generation".into()));
                }
                tx.execute("INSERT OR IGNORE INTO publications(operation_id,receipt,manifest,sequence) VALUES(?1,?2,?3,?4)",rusqlite::params![m.operation_id,serde_json::to_string(&m.receipt)?,serde_json::to_string(&m)?,m.receipt.generation.to_be_bytes().as_slice()])?;
                let saved: String = tx.query_row(
                    "SELECT manifest FROM publications WHERE operation_id=?1",
                    [&m.operation_id],
                    |r| r.get(0),
                )?;
                let saved: Manifest = serde_json::from_str(&saved)?;
                if serde_json::to_value(saved)? != serde_json::to_value(m)? {
                    return Err(Error::Corrupt(
                        "existing ledger conflicts with retained decision".into(),
                    ));
                }
            }
        }
        {
            let mut q = tx.prepare("SELECT sequence FROM publications ORDER BY sequence")?;
            let mut rows = q.query([])?;
            let mut expected = 0u64;
            while let Some(row) = rows.next()? {
                expected = expected.checked_add(1).ok_or(Error::Capacity)?;
                let sequence: Vec<u8> = row.get(0)?;
                if sequence != expected.to_be_bytes() {
                    return Err(Error::CoverageLost);
                }
            }
            if expected != generation {
                return Err(Error::CoverageLost);
            }
        }
        tx.commit()?;
        // Force registry reconstruction from canonical immutable history when it was lost.
        journal::reject_existing_changes(self, &BTreeSet::new())?;
        Ok(LedgerRebuildReport {
            publications: generation,
            generation,
            existing_rows_preserved: true,
        })
    }
}
