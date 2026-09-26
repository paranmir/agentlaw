//! C6 recovery responsibilities; shares the facade's source gate and persistence protocol.
use super::*;

impl Store {
    pub(super) fn recover_locked(&self) -> Result<()> {
        let fence = self.fence()?;
        if let Some(id) = fence.operation_id {
            validate_id(&id)?;
            let dir = self.local.join("recovery").join(id);
            let raw = fs::read(dir.join("manifest"))
                .map_err(|_| Error::RecoveryRequired("dirty fence missing manifest".into()))?;
            let md = codec::digest(&raw);
            if fence.manifest_digest.as_deref() != Some(&md) {
                return Err(Error::RecoveryRequired("manifest binding".into()));
            }
            let m: Manifest = self.load(&dir.join("manifest"))?;
            if dir.join("decision").exists() {
                let decision: String = self.load(&dir.join("decision"))?;
                if decision != md {
                    return Err(Error::RecoveryRequired("decision binding".into()));
                }
                self.redo(&dir, &m, None)?;
            } else {
                for t in &m.targets {
                    if hash_file(&self.safe_path(&t.path)?)? != t.expected {
                        return Err(Error::RecoveryRequired(
                            "undecided source divergence".into(),
                        ));
                    }
                }
                self.record(
                    &self.local.join("source-fence"),
                    &Fence {
                        generation: m.prior_generation,
                        operation_id: None,
                        manifest_digest: None,
                    },
                )?;
            }
        }
        Ok(())
    }
    pub(super) fn safe_path(&self, p: &str) -> Result<PathBuf> {
        let p = Path::new(p);
        if p.is_absolute()
            || p.components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(Error::Corrupt("unsafe manifest path".into()));
        }
        let mut resolved = self.root.clone();
        for part in p.components() {
            resolved.push(part);
            match fs::symlink_metadata(&resolved) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(Error::Corrupt("canonical symlink traversal".into()))
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(resolved)
    }
    pub(super) fn redo(&self, dir: &Path, m: &Manifest, fault: Option<FaultPoint>) -> Result<()> {
        if m.root != self.root.to_string_lossy() {
            return Err(Error::RecoveryRequired("root binding".into()));
        }
        for (i, t) in m.targets.iter().enumerate() {
            let target = self.safe_path(&t.path)?;
            let actual = hash_file(&target)?;
            if actual.as_deref() != Some(&t.digest) {
                let image = dir.join(&t.image);
                if hash_file(&image)?.as_deref() != Some(&t.digest) {
                    return Err(Error::RecoveryRequired("after-image digest".into()));
                }
                if let Some(a) = &t.append {
                    let bytes = fs::read(&image)?;
                    let mut file = OpenOptions::new().read(true).write(true).open(&target)?;
                    let length = file.metadata()?.len();
                    if length < a.offset || length > a.target_length {
                        return Err(Error::RecoveryRequired(
                            "history extra trailing/truncated prefix".into(),
                        ));
                    }
                    let mut h = Sha256::new();
                    std::io::copy(&mut Read::by_ref(&mut file).take(a.offset), &mut h)?;
                    if format!("{:x}", h.finalize()) != a.prefix_digest {
                        return Err(Error::RecoveryRequired(
                            "history immutable prefix diverged".into(),
                        ));
                    }
                    // Only the reserved suffix is mutable after a durable decision.
                    file.seek(SeekFrom::Start(a.offset))?;
                    file.write_all(
                        &bytes[usize::try_from(a.offset).map_err(|_| Error::Capacity)?..],
                    )?;
                    file.set_len(a.target_length)?;
                    file.sync_all()?;
                } else {
                    if actual != t.expected {
                        return Err(Error::RecoveryRequired(format!(
                            "source divergence: {}",
                            t.path
                        )));
                    }
                    install_reader(&target, &mut File::open(&image)?)?;
                }
                if hash_file(&target)?.as_deref() != Some(&t.digest) {
                    return Err(Error::RecoveryRequired("target verification failed".into()));
                }
            }
            hit(fault, FaultPoint::Installed(i))?;
        }
        self.record(&dir.join("published"), &m.receipt)?;
        hit(fault, FaultPoint::Published)?;
        let conn = journal::open(&self.local.join("journal.sqlite"))?;
        conn.execute(
            "INSERT OR REPLACE INTO publications(operation_id,receipt,manifest,sequence) VALUES (?1,?2,?3,?4)",
            rusqlite::params![
                m.operation_id,
                serde_json::to_string(&m.receipt)?,
                serde_json::to_string(m)?,
                m.receipt.generation.to_be_bytes().as_slice()
            ],
        )?;
        for reference in &m.receipt.references {
            let encoded = reference
                .observed_version
                .strip_prefix("av1.")
                .ok_or_else(|| Error::Corrupt("receipt version".into()))?;
            let bytes = URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| Error::Corrupt("receipt version encoding".into()))?;
            if bytes.len() != 48 {
                return Err(Error::Corrupt("receipt version length".into()));
            }
            let id = uuid::Uuid::from_slice(&bytes[..16])
                .map_err(|_| Error::Corrupt("receipt change UUID".into()))?
                .to_string();
            conn.execute(
                "INSERT OR IGNORE INTO canonical_change_ids(change_id) VALUES (?1)",
                [id],
            )?;
        }
        hit(fault, FaultPoint::Journal)?;
        if m.imported {
            conn.execute("UPDATE registry_state SET complete=0 WHERE singleton=1", [])?;
        }
        self.record(
            &self.local.join("source-fence"),
            &Fence {
                generation: m.receipt.generation,
                operation_id: None,
                manifest_digest: None,
            },
        )?;
        hit(fault, FaultPoint::Clean)?;
        Ok(())
    }
}
