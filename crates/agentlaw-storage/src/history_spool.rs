//! Disk-backed history acquisition, integrity validation, causal selection and iteration.
//! The SQLite file is a temporary owned acquisition, never a canonical replacement.
use super::*;
use rusqlite::{params, Connection, OptionalExtension};
fn initialize_spool(conn: &Connection, temporary: bool) -> Result<()> {
    if temporary {
        conn.execute_batch(
            "PRAGMA synchronous=OFF; PRAGMA journal_mode=OFF; PRAGMA temp_store=FILE;",
        )?;
    } else {
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA temp_store=FILE;",
        )?;
    }
    conn.execute_batch("CREATE TABLE frames(kind TEXT NOT NULL,key TEXT NOT NULL,change_id TEXT NOT NULL,chunk_no INTEGER NOT NULL,payload BLOB NOT NULL,sha TEXT NOT NULL,PRIMARY KEY(kind,key)); CREATE INDEX components ON frames(change_id,kind,chunk_no); CREATE TABLE changes(change_id TEXT PRIMARY KEY,entity_id TEXT NOT NULL,metadata TEXT NOT NULL,descriptor TEXT NOT NULL,checkpoint TEXT NOT NULL); CREATE INDEX entities ON changes(entity_id); CREATE TABLE edges(child TEXT NOT NULL,parent TEXT NOT NULL,owner TEXT NOT NULL,PRIMARY KEY(child,parent)); CREATE INDEX reverse_edges ON edges(parent); CREATE TABLE current_units(entity_id TEXT PRIMARY KEY,state TEXT NOT NULL,target TEXT);")?;
    Ok(())
}
fn insert_frame(conn: &Connection, info: &codec::FrameInfo, payload: &mut dyn Read) -> Result<()> {
    let mut bytes = Vec::new();
    payload.read_to_end(&mut bytes)?;
    let previous: Option<String> = conn
        .query_row(
            "SELECT sha FROM frames WHERE kind=?1 AND key=?2",
            params![info.kind, info.key],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = previous {
        if old != info.sha256 {
            return Err(Error::Corrupt("immutable frame identity collision".into()));
        }
        return Ok(());
    }
    let mut key = info.key.split('.');
    let change = key.next().unwrap();
    let chunk = key
        .next()
        .map(|n| n.parse::<u32>())
        .transpose()
        .map_err(|_| Error::Corrupt("chunk number".into()))?
        .map(i64::from)
        .unwrap_or(-1);
    conn.execute(
        "INSERT INTO frames VALUES(?1,?2,?3,?4,?5,?6)",
        params![info.kind, info.key, change, chunk, bytes, info.sha256],
    )?;
    Ok(())
}
#[derive(Debug)]
struct SpoolLease(
    PathBuf,
    bool,
    Option<File>,
    Option<std::sync::Mutex<Connection>>,
);
impl Drop for SpoolLease {
    fn drop(&mut self) {
        let _ = &self.3;
        if self.1 {
            let _ = fs::remove_file(&self.0);
        }
        if let Some(lease) = self.2.take() {
            let _ = lease.unlock();
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Component {
    #[serde(skip)]
    _lease: Option<std::sync::Arc<SpoolLease>>,
    pub spool: PathBuf,
    pub change_id: String,
    pub kind: String,
    pub bytes: u64,
    pub sha256: String,
    pub chunks: u32,
}
impl Component {
    pub fn open(&self) -> Result<ComponentReader> {
        Ok(ComponentReader {
            conn: Connection::open_with_flags(
                &self.spool,
                if self.spool.to_string_lossy().starts_with("file:") {
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                        | rusqlite::OpenFlags::SQLITE_OPEN_URI
                } else {
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                },
            )?,
            component: self.clone(),
            next: 0,
            buffer: std::io::Cursor::new(Vec::new()),
        })
    }
    pub fn materialize(&self, target: &Path) -> Result<()> {
        let mut out = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(target)?;
        let mut input = self.open()?;
        if std::io::copy(&mut input, &mut out)? != self.bytes {
            return Err(Error::Corrupt("component byte count".into()));
        }
        out.sync_all()?;
        Ok(())
    }
    pub fn read_owned(&self) -> Result<Vec<u8>> {
        if self.bytes > codec::MAX_FRAME_BYTES {
            return Err(Error::Capacity);
        }
        let mut bytes = Vec::new();
        self.open()?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}
pub struct ComponentReader {
    conn: Connection,
    component: Component,
    next: u32,
    buffer: std::io::Cursor<Vec<u8>>,
}
impl Read for ComponentReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            let n = self.buffer.read(out)?;
            if n > 0 || self.next == self.component.chunks {
                return Ok(n);
            }
            let bytes: Vec<u8> = self
                .conn
                .query_row(
                    "SELECT payload FROM frames WHERE change_id=?1 AND kind=?2 AND chunk_no=?3",
                    params![self.component.change_id, self.component.kind, self.next],
                    |r| r.get(0),
                )
                .map_err(std::io::Error::other)?;
            self.next = self
                .next
                .checked_add(1)
                .ok_or_else(|| std::io::Error::other("chunk overflow"))?;
            self.buffer = std::io::Cursor::new(bytes);
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpoolChange {
    pub metadata: Value,
    pub delta: Component,
    pub evidence: Component,
    pub body: Component,
}
impl SpoolChange {
    pub fn into_owned(self) -> Result<HistoricalState> {
        Ok(HistoricalState {
            metadata: self.metadata,
            delta: codec::parse_json(&self.delta.read_owned()?)?,
            evidence: String::from_utf8(self.evidence.read_owned()?)
                .map_err(|_| Error::Corrupt("evidence UTF8".into()))?,
            body: String::from_utf8(self.body.read_owned()?)
                .map_err(|_| Error::Corrupt("body UTF8".into()))?,
        })
    }
}
pub struct HistorySpool {
    path: PathBuf,
    conn: Connection,
    scratch_policy: delta_stream::ScratchPolicy,
    pub source_position: published::SourcePosition,
    pub cache_reused: bool,
    lease: std::sync::Arc<SpoolLease>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceAuditReport {
    pub source_position: published::SourcePosition,
    pub changes: u64,
    pub current_units: u64,
    pub validated: bool,
}
impl Store {
    pub fn spool_history(&self) -> Result<HistorySpool> {
        self.spool_history_internal(false)
    }
    pub(super) fn spool_history_internal(&self, for_audit: bool) -> Result<HistorySpool> {
        let position = {
            let _gate = self.lock()?;
            published::SourcePosition {
                epoch: self.load(&self.local.join("source-epoch"))?,
                sequence: if for_audit {
                    self.generation_for_audit()?
                } else {
                    self.ensure_clean()?
                },
            }
        };
        let cache_dir = self.local.join("history-cache");
        let cache_path = cache_dir.join(format!("{}-{}.sqlite", position.epoch, position.sequence));
        if !for_audit && cache_path.exists() {
            let lease_file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(cache_path.with_extension("lease"))?;
            lease_file.lock_shared()?;
            let conn = Connection::open_with_flags(
                &cache_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            conn.execute_batch("PRAGMA temp_store=FILE;")?;
            return Ok(HistorySpool {
                path: cache_path.clone(),
                conn,
                scratch_policy: delta_stream::ScratchPolicy::Legacy(cache_dir.clone()),
                source_position: position,
                cache_reused: true,
                lease: std::sync::Arc::new(SpoolLease(cache_path, false, Some(lease_file), None)),
            });
        }
        let dir = self.local.join("history-spools");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.sqlite", uuid::Uuid::new_v4()));
        let lease = std::sync::Arc::new(SpoolLease(path.clone(), true, None, None));
        let conn = Connection::open(&path)?;
        initialize_spool(&conn, false)?;
        let history = self.safe_path("history")?;
        if history.exists() {
            for writer in fs::read_dir(history)? {
                let writer = writer?;
                if writer.file_type()?.is_symlink() {
                    return Err(Error::Corrupt("history writer symlink".into()));
                }
                for entry in fs::read_dir(writer.path())? {
                    let entry = entry?;
                    if entry.file_type()?.is_symlink() {
                        return Err(Error::Corrupt("history pack symlink".into()));
                    }
                    let pack = entry.path();
                    if pack.extension().is_some_and(|e| e == "tmp") {
                        continue;
                    }
                    let _gate = self.lock()?;
                    if self.generation_for_audit()? != position.sequence {
                        return Err(Error::Stale(
                            "source changed while history was acquired".into(),
                        ));
                    }
                    codec::scan(
                        "history",
                        &mut BufReader::new(File::open(pack)?),
                        |info, payload| insert_frame(&conn, info, payload),
                    )?;
                }
            }
        }
        let mut spool = HistorySpool {
            lease,
            cache_reused: false,
            path,
            conn,
            scratch_policy: delta_stream::ScratchPolicy::Legacy(dir.clone()),
            source_position: position,
        };
        spool.validate_history()?;
        if !for_audit {
            let _gate = self.lock()?;
            if self.ensure_clean()? != spool.source_position.sequence {
                return Err(Error::Stale(
                    "source changed before history cache publication".into(),
                ));
            }
            fs::create_dir_all(cache_dir)?;
            install_reader(&cache_path, &mut File::open(&spool.path)?)?;
        }
        Ok(spool)
    }
    pub fn visit_history_closure(
        &self,
        entity_id: &str,
        mut visit: impl FnMut(SpoolChange) -> Result<()>,
    ) -> Result<u64> {
        let mut spool = self.spool_history()?;
        spool.visit_closure(entity_id, |c| visit(c))
    }
    pub fn audit_source(&self) -> Result<SourceAuditReport> {
        let mut spool = self.spool_history_internal(true)?;
        let report = spool.audit_current(self)?;
        let base = self.safe_path("catalog/projects")?;
        if base.exists() {
            for shard in fs::read_dir(base)? {
                let shard = shard?;
                if shard.file_type()?.is_symlink() {
                    return Err(Error::Corrupt("catalog shard symlink".into()));
                }
                for entry in fs::read_dir(shard.path())? {
                    let path = entry?.path();
                    if path.extension().is_some_and(|e| e == "tmp") {
                        continue;
                    }
                    let _gate = self.lock()?;
                    if self.generation_for_audit()? != report.source_position.sequence {
                        return Err(Error::Stale("source changed during catalog audit".into()));
                    }
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| Error::Corrupt("catalog filename".into()))?;
                    validate_id(id)?;
                    let canonical =
                        self.safe_path(&format!("catalog/projects/{}/{}.md", &id[..2], id))?;
                    if canonical != path {
                        return Err(Error::Corrupt("catalog path identity".into()));
                    }
                    let frames = codec::read("catalog", &mut BufReader::new(File::open(&path)?))?;
                    if frames.len() != 1
                        || frames[0].kind != "project"
                        || frames[0].key != id
                        || codec::parse_json(&frames[0].payload)?["project_id"] != id
                    {
                        return Err(Error::Corrupt("catalog identity/cardinality".into()));
                    }
                }
            }
        }
        Ok(report)
    }
}
impl HistorySpool {
    pub(super) fn assert_history_preserved(&self, previous: &HistorySpool) -> Result<()> {
        self.conn.execute(
            "ATTACH DATABASE ?1 AS previous_history",
            [previous.path.to_string_lossy().as_ref()],
        )?;
        let bad:bool=self.conn.query_row("SELECT EXISTS(SELECT 1 FROM previous_history.frames old LEFT JOIN frames new ON new.kind=old.kind AND new.key=old.key WHERE new.key IS NULL OR new.sha<>old.sha)",[],|r|r.get(0))?;
        self.conn
            .execute_batch("DETACH DATABASE previous_history")?;
        if bad {
            return Err(Error::Corrupt(
                "import omitted or altered immutable source history".into(),
            ));
        }
        Ok(())
    }
    pub(super) fn is_ancestor_of_any(&self, ancestor: &str, heads: &[String]) -> Result<bool> {
        for head in heads {
            let yes:bool=self.conn.query_row("WITH RECURSIVE ancestors(id) AS (SELECT ?1 UNION SELECT edges.parent FROM edges JOIN ancestors ON edges.child=ancestors.id) SELECT EXISTS(SELECT 1 FROM ancestors WHERE id=?2)",params![head,ancestor],|r|r.get(0))?;
            if yes {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Size of the acquired SQLite database, regardless of its backing store.
    pub fn size_bytes(&self) -> Result<u64> {
        let pages: u64 = self
            .conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: u64 = self
            .conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))?;
        pages.checked_mul(page_size).ok_or(Error::Capacity)
    }
    /// Move an acquired in-memory spool into its private unsynchronized file.
    /// Existing component handles keep their previous backing alive until dropped.
    pub fn spill_to_file(&mut self) -> Result<()> {
        if !self.path.to_string_lossy().starts_with("file:") {
            return Ok(());
        }
        let directory = match &self.scratch_policy {
            delta_stream::ScratchPolicy::Bounded { directory, .. } => directory.clone(),
            delta_stream::ScratchPolicy::Legacy(directory) => directory.clone(),
        };
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{}.sqlite", uuid::Uuid::new_v4()));
        let lease = std::sync::Arc::new(SpoolLease(path.clone(), true, None, None));
        self.conn
            .execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])?;
        let connection = Connection::open(&path)?;
        connection.execute_batch(
            "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE;",
        )?;
        self.conn = connection;
        self.path = path;
        self.lease = lease;
        self.scratch_policy = delta_stream::ScratchPolicy::Bounded {
            memory: 0,
            directory,
        };
        Ok(())
    }
    /// An exact retained historical input for the import-only resolution writer.
    pub fn exact_change(&self, change_id: &str) -> Result<SpoolChange> {
        validate_id(change_id)?;
        self.change(change_id)
    }
    fn component(&self, id: &str, kind: &str, desc: &Value) -> Result<Component> {
        self.component_mode(id, kind, desc, true)
    }
    fn component_mode(
        &self,
        id: &str,
        kind: &str,
        desc: &Value,
        verify: bool,
    ) -> Result<Component> {
        let length = desc["bytes"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("component length".into()))?
            .parse::<u64>()
            .map_err(|_| Error::Corrupt("component length overflow".into()))?;
        let count = desc["chunks"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| Error::Corrupt("component chunk count".into()))?;
        let digest = desc["sha256"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("component checksum".into()))?
            .to_owned();
        let (actual, max): (u64, Option<u32>) = self.conn.query_row(
            "SELECT count(*),max(chunk_no) FROM frames WHERE change_id=?1 AND kind=?2",
            params![id, kind],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if actual != u64::from(count) || count == 0 || max != Some(count - 1) {
            return Err(Error::Corrupt("component missing/extra chunks".into()));
        }
        let c = Component {
            _lease: Some(self.lease.clone()),
            spool: self.path.clone(),
            change_id: id.into(),
            kind: kind.into(),
            bytes: length,
            sha256: digest,
            chunks: count,
        };
        if !verify {
            return Ok(c);
        }
        let mut h = Sha256::new();
        let n = std::io::copy(&mut c.open()?, &mut h)?;
        if n != c.bytes || format!("{:x}", h.finalize()) != c.sha256 {
            return Err(Error::Corrupt("assembled component checksum/length".into()));
        }
        Ok(c)
    }
    fn validate_history(&mut self) -> Result<()> {
        let mut query = self.conn.prepare(
            "SELECT key,payload FROM frames WHERE kind='change_descriptor' ORDER BY key",
        )?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let d = codec::parse_json(&row.get::<_, Vec<u8>>(1)?)?;
            if d["change_id"] != id {
                return Err(Error::Corrupt("change descriptor identity".into()));
            }
            let metadata_component =
                self.component(&id, "change_metadata_chunk", &d["metadata"])?;
            let metadata = codec::parse_json(&metadata_component.read_owned()?)?;
            let entity = metadata["entity_id"]
                .as_str()
                .ok_or_else(|| Error::Corrupt("history entity".into()))?;
            validate_id(entity)?;
            if metadata["change_id"] != id || d["entity_id"] != entity {
                return Err(Error::Corrupt("history identity mismatch".into()));
            }
            self.component(&id, "delta_chunk", &d["delta"])?;
            self.component(&id, "evidence_chunk", &d["evidence"])?;
            let cpbytes: Vec<u8> = self
                .conn
                .query_row(
                    "SELECT payload FROM frames WHERE kind='checkpoint_descriptor' AND key=?1",
                    [&id],
                    |r| r.get(0),
                )
                .map_err(|_| Error::Corrupt("checkpoint missing".into()))?;
            let cp = codec::parse_json(&cpbytes)?;
            let body = self.component(&id, "checkpoint_body_chunk", &cp["body"])?;
            let cm = self.component(&id, "checkpoint_metadata_chunk", &cp["metadata"])?;
            let head_metadata = codec::parse_json(&cm.read_owned()?)?;
            if head_metadata != metadata["metadata_after"]
                || metadata["result_body_sha256"] != body.sha256
                || metadata["result_body_bytes"] != body.bytes.to_string()
            {
                return Err(Error::Corrupt("checkpoint/result mismatch".into()));
            }
            let head = Head {
                metadata: head_metadata,
                body: String::new(),
            };
            let unit = CurrentUnit {
                entity_id: entity.into(),
                entity_type: metadata["entity_type"].as_str().unwrap_or("").into(),
                state: UnitState::Live {
                    heads: vec![head.clone()],
                },
            };
            unit.validate()?;
            let version = version_stream(&unit, &head, body.bytes, &mut body.open()?)?;
            if metadata["observed_version"] != version || cp["observed_version"] != version {
                return Err(Error::Corrupt("history state digest".into()));
            }
            self.conn.execute(
                "INSERT INTO changes VALUES(?1,?2,?3,?4,?5)",
                params![
                    id,
                    entity,
                    serde_json::to_string(&metadata)?,
                    serde_json::to_string(&d)?,
                    serde_json::to_string(&cp)?
                ],
            )?;
            let parents = metadata["parent_change_ids"]
                .as_array()
                .ok_or_else(|| Error::Corrupt("parents".into()))?;
            let owners = metadata["parent_owners"]
                .as_array()
                .ok_or_else(|| Error::Corrupt("parent owners".into()))?;
            if parents.len() != owners.len() {
                return Err(Error::Corrupt("parent owner cardinality".into()));
            }
            for parent in parents {
                let p = parent
                    .as_str()
                    .ok_or_else(|| Error::Corrupt("parent ID".into()))?;
                validate_id(p)?;
                if p == id {
                    return Err(Error::Corrupt("self parent".into()));
                }
                let matches = owners
                    .iter()
                    .filter(|o| o["change_id"] == p)
                    .collect::<Vec<_>>();
                if matches.len() != 1 {
                    return Err(Error::Corrupt("parent owner mapping".into()));
                }
                let owner = matches[0]["entity_id"]
                    .as_str()
                    .ok_or_else(|| Error::Corrupt("parent owner".into()))?;
                validate_id(owner)?;
                self.conn
                    .execute("INSERT INTO edges VALUES(?1,?2,?3)", params![id, p, owner])
                    .map_err(|_| Error::Corrupt("duplicate parent".into()))?;
            }
            if parents.len() > 1 {
                let base = metadata["delta_base_id"]
                    .as_str()
                    .ok_or_else(|| Error::Corrupt("merge delta base missing".into()))?;
                if !parents.iter().any(|p| p == base) {
                    return Err(Error::Corrupt("delta base not parent".into()));
                }
            } else if metadata.get("delta_base_id").is_some() {
                return Err(Error::Corrupt("unexpected delta base".into()));
            }
        }
        drop(rows);
        drop(query);
        let orphan:bool=self.conn.query_row("SELECT EXISTS(SELECT 1 FROM frames f LEFT JOIN changes c ON c.change_id=f.change_id WHERE c.change_id IS NULL OR f.kind NOT IN ('change_descriptor','checkpoint_descriptor','change_metadata_chunk','delta_chunk','evidence_chunk','checkpoint_body_chunk','checkpoint_metadata_chunk'))",[],|r|r.get(0))?;
        if orphan {
            return Err(Error::Corrupt("orphan/unknown immutable frame".into()));
        }
        let invalid:bool=self.conn.query_row("SELECT EXISTS(SELECT 1 FROM edges e LEFT JOIN changes p ON p.change_id=e.parent WHERE p.change_id IS NULL OR p.entity_id<>e.owner)",[],|r|r.get(0))?;
        if invalid {
            return Err(Error::Corrupt(
                "missing parent or wrong parent owner".into(),
            ));
        }
        self.select_all()?;
        self.topological(|_| Ok(()))?;
        Ok(())
    }
    fn select_all(&mut self) -> Result<()> {
        self.conn.execute_batch("DROP TABLE IF EXISTS selected; CREATE TEMP TABLE selected(change_id TEXT PRIMARY KEY,pending INTEGER NOT NULL DEFAULT 0,emitted INTEGER NOT NULL DEFAULT 0); INSERT INTO selected(change_id) SELECT change_id FROM changes;")?;
        Ok(())
    }
    pub fn visit_closure(
        &mut self,
        entity_id: &str,
        visit: impl FnMut(SpoolChange) -> Result<()>,
    ) -> Result<u64> {
        validate_id(entity_id)?;
        self.conn.execute_batch("DROP TABLE IF EXISTS selected; CREATE TEMP TABLE selected(change_id TEXT PRIMARY KEY,pending INTEGER NOT NULL DEFAULT 0,emitted INTEGER NOT NULL DEFAULT 0);")?;
        self.conn.execute("INSERT INTO selected(change_id) WITH RECURSIVE closure(id) AS (SELECT change_id FROM changes WHERE entity_id=?1 UNION SELECT e.parent FROM edges e JOIN closure c ON c.id=e.child) SELECT id FROM closure",[entity_id])?;
        self.topological(visit)
    }
    /// Causal metadata plus lazy component handles; validated cache bytes are not reread.
    pub fn visit_metadata_closure(
        &mut self,
        entity_id: &str,
        visit: impl FnMut(SpoolChange) -> Result<()>,
    ) -> Result<u64> {
        validate_id(entity_id)?;
        self.conn.execute_batch("DROP TABLE IF EXISTS selected; CREATE TEMP TABLE selected(change_id TEXT PRIMARY KEY,pending INTEGER NOT NULL DEFAULT 0,emitted INTEGER NOT NULL DEFAULT 0);")?;
        self.conn.execute("INSERT INTO selected(change_id) WITH RECURSIVE closure(id) AS (SELECT change_id FROM changes WHERE entity_id=?1 UNION SELECT e.parent FROM edges e JOIN closure c ON c.id=e.child) SELECT id FROM closure",[entity_id])?;
        self.topological_mode(visit, false)
    }
    fn describe_change(&self, id: &str) -> Result<SpoolChange> {
        let (m, d, c): (String, String, String) = self.conn.query_row(
            "SELECT metadata,descriptor,checkpoint FROM changes WHERE change_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let d = codec::parse_json(d.as_bytes())?;
        let c = codec::parse_json(c.as_bytes())?;
        Ok(SpoolChange {
            metadata: codec::parse_json(m.as_bytes())?,
            delta: self.component_mode(id, "delta_chunk", &d["delta"], false)?,
            evidence: self.component_mode(id, "evidence_chunk", &d["evidence"], false)?,
            body: self.component_mode(id, "checkpoint_body_chunk", &c["body"], false)?,
        })
    }
    fn change(&self, id: &str) -> Result<SpoolChange> {
        let (m, d, c): (String, String, String) = self.conn.query_row(
            "SELECT metadata,descriptor,checkpoint FROM changes WHERE change_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let metadata = codec::parse_json(m.as_bytes())?;
        let d = codec::parse_json(d.as_bytes())?;
        let cp = codec::parse_json(c.as_bytes())?;
        let change = SpoolChange {
            metadata,
            delta: self.component(id, "delta_chunk", &d["delta"])?,
            evidence: self.component(id, "evidence_chunk", &d["evidence"])?,
            body: self.component(id, "checkpoint_body_chunk", &cp["body"])?,
        };
        let parents = change.metadata["parent_change_ids"]
            .as_array()
            .ok_or_else(|| Error::Corrupt("parents".into()))?;
        let base_id = if parents.len() > 1 {
            change.metadata["delta_base_id"].as_str()
        } else {
            parents.first().and_then(Value::as_str)
        };
        let base: Box<dyn Read> = if let Some(id) = base_id {
            let cp: String = self.conn.query_row(
                "SELECT checkpoint FROM changes WHERE change_id=?1",
                [id],
                |r| r.get(0),
            )?;
            let cp = codec::parse_json(cp.as_bytes())?;
            Box::new(
                self.component(id, "checkpoint_body_chunk", &cp["body"])?
                    .open()?,
            )
        } else {
            Box::new(std::io::empty())
        };
        delta_stream::verify(
            change.delta.open()?,
            base,
            change.body.open()?,
            &self.scratch_policy,
        )?;
        Ok(change)
    }
    fn topological(&mut self, visit: impl FnMut(SpoolChange) -> Result<()>) -> Result<u64> {
        self.topological_mode(visit, true)
    }
    fn topological_mode(
        &mut self,
        mut visit: impl FnMut(SpoolChange) -> Result<()>,
        verify: bool,
    ) -> Result<u64> {
        self.conn.execute_batch("UPDATE selected SET pending=(SELECT count(*) FROM edges e JOIN selected p ON p.change_id=e.parent WHERE e.child=selected.change_id),emitted=0; CREATE INDEX IF NOT EXISTS ready ON selected(emitted,pending,change_id);")?;
        let total: u64 = self
            .conn
            .query_row("SELECT count(*) FROM selected", [], |r| r.get(0))?;
        let mut count = 0u64;
        loop {
            let next:Option<String>=self.conn.query_row("SELECT change_id FROM selected WHERE emitted=0 AND pending=0 ORDER BY change_id LIMIT 1",[],|r|r.get(0)).optional()?;
            let Some(id) = next else { break };
            visit(if verify {
                self.change(&id)?
            } else {
                self.describe_change(&id)?
            })?;
            self.conn
                .execute("UPDATE selected SET emitted=1 WHERE change_id=?1", [&id])?;
            self.conn.execute("UPDATE selected SET pending=pending-1 WHERE change_id IN (SELECT child FROM edges WHERE parent=?1)",[id])?;
            count = count.checked_add(1).ok_or(Error::Capacity)?;
        }
        if count != total {
            return Err(Error::Corrupt("history DAG cycle".into()));
        }
        Ok(count)
    }
    pub fn audit_current(&mut self, store: &Store) -> Result<SourceAuditReport> {
        self.conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS current_units(entity_id TEXT PRIMARY KEY,state TEXT NOT NULL,target TEXT); DELETE FROM temp.current_units;")?;
        self.conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS current_head_ids(entity_id TEXT,change_id TEXT); DELETE FROM current_head_ids; CREATE TEMP TABLE IF NOT EXISTS redirect_consolidations(entity_id TEXT,change_id TEXT,target TEXT); DELETE FROM redirect_consolidations;")?;
        let mut count = 0u64;
        for kind in ["memory", "procedure"] {
            let base = store.root.join("current").join(kind);
            if !base.exists() {
                continue;
            }
            for shard in fs::read_dir(base)? {
                for entry in fs::read_dir(shard?.path())? {
                    let path = entry?.path();
                    if path.extension().is_some_and(|s| s == "tmp") {
                        continue;
                    }
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| Error::Corrupt("current path".into()))?;
                    let acquired = {
                        let _gate = store.lock()?;
                        let position = published::SourcePosition {
                            epoch: store.load(&store.local.join("source-epoch"))?,
                            sequence: store.generation_for_audit()?,
                        };
                        store.acquire_kind_locked(id, kind, position)?
                    };
                    if acquired.source_position != self.source_position {
                        return Err(Error::Stale("source changed during audit".into()));
                    }
                    for reference in &acquired.references {
                        let bytes = URL_SAFE_NO_PAD
                            .decode(reference.observed_version.strip_prefix("av1.").unwrap())
                            .map_err(|_| Error::Corrupt("version".into()))?;
                        let change = uuid::Uuid::from_slice(&bytes[..16])
                            .map_err(|_| Error::Corrupt("change ID".into()))?
                            .to_string();
                        let value: Option<String> = self
                            .conn
                            .query_row(
                                "SELECT metadata FROM changes WHERE change_id=?1",
                                [change],
                                |r| r.get(0),
                            )
                            .optional()?;
                        let metadata = codec::parse_json(
                            value
                                .ok_or_else(|| {
                                    Error::Corrupt("current head missing history".into())
                                })?
                                .as_bytes(),
                        )?;
                        if metadata["observed_version"] != reference.observed_version
                            || metadata["entity_id"] != id
                        {
                            return Err(Error::Corrupt("current/history disagreement".into()));
                        }
                    }
                    for head in acquired.state["current_heads"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        self.conn.execute(
                            "INSERT INTO current_head_ids VALUES(?1,?2)",
                            params![id, head["change_id"].as_str()],
                        )?;
                    }
                    if acquired.state["state"] == "redirect" {
                        self.conn.execute(
                            "INSERT INTO redirect_consolidations VALUES(?1,?2,?3)",
                            params![
                                id,
                                acquired.state["consolidation_change_id"].as_str(),
                                acquired.state["redirect_to"].as_str()
                            ],
                        )?;
                    }
                    self.conn.execute(
                        "INSERT INTO current_units VALUES(?1,?2,?3)",
                        params![
                            id,
                            acquired.state["state"].as_str().unwrap_or(""),
                            acquired.state["redirect_to"].as_str()
                        ],
                    )?;
                    for b in acquired.bodies.values() {
                        fs::remove_file(&b.path)?;
                    }
                    count = count.checked_add(1).ok_or(Error::Capacity)?;
                }
            }
        }
        let invalid:bool=self.conn.query_row("SELECT EXISTS(SELECT 1 FROM current_units c LEFT JOIN current_units d ON d.entity_id=c.target WHERE c.state='redirect' AND d.entity_id IS NULL)",[],|r|r.get(0))?;
        if invalid {
            return Err(Error::Corrupt("redirect destination absent".into()));
        }
        let mut q = self
            .conn
            .prepare("SELECT entity_id FROM current_units WHERE state='redirect'")?;
        let mut rows = q.query([])?;
        while let Some(row) = rows.next()? {
            let mut id: String = row.get(0)?;
            let origin = id.clone();
            let mut steps = 0u64;
            loop {
                let target: Option<String> = self.conn.query_row(
                    "SELECT target FROM current_units WHERE entity_id=?1",
                    [&id],
                    |r| r.get(0),
                )?;
                let Some(next) = target else { break };
                steps = steps.checked_add(1).ok_or(Error::Capacity)?;
                if steps > count {
                    return Err(Error::Corrupt("redirect cycle".into()));
                }
                id = next;
            }
            let (change, target): (String, String) = self.conn.query_row(
                "SELECT change_id,target FROM redirect_consolidations WHERE entity_id=?1",
                [&origin],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let owner: Option<String> = self
                .conn
                .query_row(
                    "SELECT entity_id FROM changes WHERE change_id=?1",
                    [&change],
                    |r| r.get(0),
                )
                .optional()?;
            if owner.as_deref() != Some(target.as_str()) {
                return Err(Error::Corrupt(
                    "redirect consolidation missing or has wrong destination owner".into(),
                ));
            }
            let incorporates_source: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM edges WHERE child=?1 AND owner=?2)",
                params![change, origin],
                |r| r.get(0),
            )?;
            if !incorporates_source {
                return Err(Error::Corrupt(
                    "redirect consolidation does not incorporate its source memory".into(),
                ));
            }
            let mut heads = self
                .conn
                .prepare("SELECT change_id FROM current_head_ids WHERE entity_id=?1")?;
            let heads = heads
                .query_map([&id], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if !self.is_ancestor_of_any(&change, &heads)? {
                return Err(Error::Corrupt(
                    "redirect consolidation is not an ancestor of its resolved current destination"
                        .into(),
                ));
            }
        }
        let changes: u64 = self
            .conn
            .query_row("SELECT count(*) FROM changes", [], |r| r.get(0))?;
        Ok(SourceAuditReport {
            source_position: self.source_position.clone(),
            changes,
            current_units: count,
            validated: true,
        })
    }
}

const HISTORY_MEMORY_BUDGET: u64 = 64 * 1024 * 1024;

enum AcquiredPack {
    Memory(Vec<u8>),
    File(PathBuf),
}

/// Owned pack bytes pinned to the source position of the read guard.
pub struct AcquiredHistory {
    packs: Vec<AcquiredPack>,
    files: Vec<SpoolLease>,
    local: PathBuf,
    position: published::SourcePosition,
    spilled: bool,
    scratch_budget: u64,
}

impl published::SourceReadGuard<'_> {
    pub fn acquire_history(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<AcquiredHistory> {
        self.acquire_history_with_budget(cancel, HISTORY_MEMORY_BUDGET)
    }

    fn acquire_history_with_budget(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
        budget: u64,
    ) -> Result<AcquiredHistory> {
        self.acquire_history_observed(cancel, budget, || {})
    }

    fn acquire_history_observed(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
        budget: u64,
        mut after_pack: impl FnMut(),
    ) -> Result<AcquiredHistory> {
        use std::sync::atomic::Ordering;
        let mut acquired = AcquiredHistory {
            packs: Vec::new(),
            files: Vec::new(),
            local: self.store.local.clone(),
            position: self.position.clone(),
            spilled: false,
            scratch_budget: budget,
        };
        let mut used = 0u64;
        let check = || {
            if cancel.load(Ordering::Acquire) {
                Err(Error::CancelledBeforeDecision)
            } else {
                Ok(())
            }
        };
        check()?;
        let history = self.store.safe_path("history")?;
        if history.exists() {
            for writer in fs::read_dir(history)? {
                let writer = writer?;
                if writer.file_type()?.is_symlink() {
                    return Err(Error::Corrupt("history writer symlink".into()));
                }
                for entry in fs::read_dir(writer.path())? {
                    check()?;
                    let entry = entry?;
                    if entry.file_type()?.is_symlink() {
                        return Err(Error::Corrupt("history pack symlink".into()));
                    }
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "tmp") {
                        continue;
                    }
                    let size = entry.metadata()?.len();
                    if !acquired.spilled && used.checked_add(size).is_none_or(|n| n > budget) {
                        acquired.spill_packs()?;
                    }
                    if acquired.spilled {
                        let target = acquired.new_file("pack")?;
                        let mut out = OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&target)?;
                        std::io::copy(&mut File::open(path)?, &mut out)?;
                        acquired.packs.push(AcquiredPack::File(target));
                    } else {
                        let bytes = fs::read(path)?;
                        used = used
                            .checked_add(bytes.len() as u64)
                            .ok_or(Error::Capacity)?;
                        if used > budget {
                            return Err(Error::Capacity);
                        }
                        acquired.packs.push(AcquiredPack::Memory(bytes));
                    }
                    after_pack();
                }
            }
        }
        check()?;
        acquired.scratch_budget = budget;
        Ok(acquired)
    }
}

impl AcquiredHistory {
    fn new_file(&mut self, extension: &str) -> Result<PathBuf> {
        let dir = self.local.join("history-spools");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.{}", uuid::Uuid::new_v4(), extension));
        self.files.push(SpoolLease(path.clone(), true, None, None));
        Ok(path)
    }

    fn spill_packs(&mut self) -> Result<()> {
        self.spilled = true;
        for pack in std::mem::take(&mut self.packs) {
            match pack {
                AcquiredPack::Memory(bytes) => {
                    let file = self.new_file("pack")?;
                    fs::write(&file, bytes)?;
                    self.packs.push(AcquiredPack::File(file));
                }
                AcquiredPack::File(path) => self.packs.push(AcquiredPack::File(path)),
            }
        }
        Ok(())
    }

    pub fn into_spool(self) -> Result<HistorySpool> {
        self.into_spool_observed(|_| {})
    }

    fn into_spool_observed(mut self, before_loading: impl FnOnce(&Self)) -> Result<HistorySpool> {
        // SQLite pages, indexes and validation rows can exceed the packed payload.
        // Reserve room for them as well as the still-owned input and frame buffers.
        let raw = self
            .packs
            .iter()
            .map(|pack| match pack {
                AcquiredPack::Memory(bytes) => bytes.len() as u64,
                AcquiredPack::File(_) => 0,
            })
            .sum::<u64>();
        let reserved = raw.saturating_mul(4).saturating_add(1024 * 1024);
        let disk = self.spilled || reserved > self.scratch_budget;
        if disk && !self.spilled {
            self.spill_packs()?;
        }
        before_loading(&self);
        let (path, conn, lease) = if disk {
            let path = self.new_file("sqlite")?;
            // The database lease outlives acquisition files and all component readers.
            self.files.pop();
            let lease = std::sync::Arc::new(SpoolLease(path.clone(), true, None, None));
            let conn = Connection::open(&path)?;
            (path, conn, lease)
        } else {
            let path = PathBuf::from(format!(
                "file:history-{}?mode=memory&cache=shared",
                uuid::Uuid::new_v4()
            ));
            let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_URI;
            let conn = Connection::open_with_flags(&path, flags)?;
            let keeper = Connection::open_with_flags(&path, flags)?;
            let lease = std::sync::Arc::new(SpoolLease(
                path.clone(),
                false,
                None,
                Some(std::sync::Mutex::new(keeper)),
            ));
            (path, conn, lease)
        };
        let mut spool = HistorySpool {
            path,
            conn,
            scratch_policy: delta_stream::ScratchPolicy::Bounded {
                memory: if disk {
                    0
                } else {
                    self.scratch_budget - reserved
                },
                directory: self.local.join("history-spools"),
            },
            source_position: self.position.clone(),
            cache_reused: false,
            lease,
        };
        initialize_spool(&spool.conn, true)?;
        for pack in std::mem::take(&mut self.packs) {
            match pack {
                AcquiredPack::Memory(bytes) => {
                    codec::scan(
                        "history",
                        &mut BufReader::new(std::io::Cursor::new(bytes)),
                        |info, payload| insert_frame(&spool.conn, info, payload),
                    )?;
                }
                AcquiredPack::File(path) => {
                    codec::scan(
                        "history",
                        &mut BufReader::new(File::open(&path)?),
                        |info, payload| insert_frame(&spool.conn, info, payload),
                    )?;
                    if let Some(index) = self.files.iter().position(|lease| lease.0 == path) {
                        drop(self.files.swap_remove(index));
                    }
                }
            }
        }
        spool.validate_history()?;
        Ok(spool)
    }
}

#[cfg(test)]
mod tests;
