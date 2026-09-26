//! Rebuildable, disk-backed exact membership index. No current body inventory is
//! retained in Rust; source publication pages update only changed identities.
use crate::*;
use agentlaw_storage::{
    published::{PublishedChangeSource, SourcePosition},
    CurrentUnit, UnitState,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
fn db<T>(r: std::result::Result<T, rusqlite::Error>) -> Result<T> {
    r.map_err(|_| {
        DomainError::new(
            "exact_index_failed",
            "The rebuildable exact-membership index is unavailable.",
        )
    })
}
fn u64_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        DomainError::new(
            "source_counter_overflow",
            "Index source generation exceeds the supported counter.",
        )
    })
}
pub struct ExactIndex {
    connection: Connection,
    directory: PathBuf,
}
#[derive(Clone, Debug)]
pub struct HeadRecord {
    pub entity_id: String,
    pub entity_type: String,
    pub observed_version: String,
    pub metadata: Value,
    pub excerpt: String,
}
impl ExactIndex {
    fn maintenance_gate(&self) -> Result<Connection> {
        let gate = db(Connection::open(
            self.directory.join("exact-maintenance.sqlite"),
        ))?;
        db(gate.busy_timeout(std::time::Duration::from_secs(30)))?;
        db(gate.execute_batch("BEGIN IMMEDIATE"))?;
        Ok(gate)
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let directory = path
            .as_ref()
            .parent()
            .ok_or_else(|| DomainError::new("exact_index_failed", "Index directory is missing."))?
            .to_path_buf();
        let connection = db(Connection::open(path))?;
        db(connection.busy_timeout(std::time::Duration::from_secs(5)))?;
        db(connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA temp_store=FILE;
        CREATE TABLE IF NOT EXISTS exact_position(singleton INTEGER PRIMARY KEY CHECK(singleton=1),epoch TEXT NOT NULL,sequence TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS exact_lexical(singleton INTEGER PRIMARY KEY CHECK(singleton=1),filename TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS exact_units(id TEXT PRIMARY KEY,kind TEXT NOT NULL,redirect TEXT);
        CREATE TABLE IF NOT EXISTS exact_heads(id TEXT NOT NULL,version TEXT NOT NULL,kind TEXT NOT NULL,scope TEXT NOT NULL,is_rule INTEGER,task INTEGER,metadata TEXT NOT NULL,PRIMARY KEY(id,version));
        CREATE INDEX IF NOT EXISTS exact_scope ON exact_heads(scope,is_rule,task);
        CREATE TABLE IF NOT EXISTS exact_bodies(id TEXT NOT NULL,version TEXT NOT NULL,digest TEXT NOT NULL,PRIMARY KEY(id,version));
        CREATE TABLE IF NOT EXISTS exact_previews(id TEXT NOT NULL,version TEXT NOT NULL,excerpt TEXT NOT NULL,PRIMARY KEY(id,version));
        CREATE INDEX IF NOT EXISTS exact_body_digest ON exact_bodies(digest);
        CREATE TABLE IF NOT EXISTS exact_edges(source TEXT NOT NULL,version TEXT NOT NULL,target TEXT NOT NULL,kind TEXT NOT NULL,PRIMARY KEY(source,version,target,kind));
        CREATE INDEX IF NOT EXISTS reverse_required ON exact_edges(target,kind);
        CREATE TABLE IF NOT EXISTS exact_targets(source TEXT NOT NULL,version TEXT NOT NULL,project TEXT NOT NULL,path TEXT NOT NULL,kind TEXT NOT NULL,reading TEXT NOT NULL,PRIMARY KEY(source,version,project,path,kind,reading));
        CREATE INDEX IF NOT EXISTS target_project ON exact_targets(project,path);
    "))?;
        Ok(Self {
            connection,
            directory,
        })
    }
    fn lexical(&self, epoch: &str) -> Result<agentlaw_search::SearchIndex> {
        let name: Option<String> = db(self
            .connection
            .query_row(
                "SELECT filename FROM exact_lexical WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional())?;
        let name = name
            .unwrap_or_else(|| format!("lexical-{:x}.sqlite", Sha256::digest(epoch.as_bytes())));
        agentlaw_search::SearchIndex::open(self.directory.join(name)).map_err(|_| {
            DomainError::new(
                "lexical_index_failed",
                "The derived lexical index is unavailable.",
            )
        })
    }
    pub fn position(&self) -> Result<Option<SourcePosition>> {
        let value: Option<(String, String)> = db(self
            .connection
            .query_row(
                "SELECT epoch,sequence FROM exact_position WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional())?;
        value
            .map(|(epoch, seq)| {
                Ok(SourcePosition {
                    epoch,
                    sequence: seq.parse().map_err(|_| {
                        DomainError::new("exact_index_corrupt", "Invalid exact-index watermark.")
                    })?,
                })
            })
            .transpose()
    }
    pub fn identical_bodies(&self, body: &str) -> Result<Vec<String>> {
        let mut statement = db(self
            .connection
            .prepare("SELECT DISTINCT id FROM exact_bodies WHERE digest=?1 ORDER BY id"))?;
        let rows = db(statement
            .query_map([format!("{:x}", Sha256::digest(body.as_bytes()))], |r| {
                r.get::<_, String>(0)
            }))?;
        db(rows.collect())
    }
    pub fn synchronize(
        &mut self,
        source: &agentlaw_storage::Store,
        control: &RequestControl,
    ) -> Result<SourcePosition> {
        let _maintenance = self.maintenance_gate()?;
        control.phase("synchronizing");
        let reader = source.owned_published_reader();
        let now = reader.position().map_err(|_| {
            DomainError::new("source_unavailable", "Cannot inspect source position.")
        })?;
        let mut cursor = self.position()?.ok_or_else(|| {
            DomainError::new(
                "exact_index_rebuild_required",
                "A verified initial inventory is required.",
            )
        })?;
        if cursor.epoch != now.epoch {
            return Err(DomainError::new(
                "exact_index_rebuild_required",
                "Source epoch changed.",
            ));
        }
        while cursor.sequence < now.sequence {
            control.check()?;
            let page = reader.read_published_paths(&cursor, 1).map_err(|_| {
                DomainError::new(
                    "exact_index_coverage_unknown",
                    "Canonical publication coverage is incomplete.",
                )
            })?;
            let mut acquired = Vec::new();
            for path in &page.canonical_paths {
                let procedure = path.starts_with("current/procedure/");
                if !procedure && !path.starts_with("current/memory/") {
                    continue;
                }
                let id = std::path::Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| DomainError::new("source_corrupt", "Invalid canonical path."))?;
                let unit = if procedure {
                    reader.acquire_procedure(id)
                } else {
                    reader.acquire_current(id)
                }
                .map_err(|_| {
                    DomainError::new("source_unavailable", "Cannot acquire changed identity.")
                })?;
                if unit.source_position != page.source_position {
                    return Err(DomainError::new(
                        "source_changed",
                        "Source changed during incremental acquisition.",
                    ));
                }
                acquired.push(unit);
            }
            if reader.position().map_err(|_| {
                DomainError::new("source_unavailable", "Cannot verify source position.")
            })? != page.source_position
            {
                return Err(DomainError::new(
                    "source_changed",
                    "Source changed during incremental acquisition.",
                ));
            }
            let units = acquired
                .iter()
                .map(acquired_unit)
                .collect::<Result<Vec<_>>>()?;
            let mut lexical = self.lexical(&cursor.epoch)?;
            let through = u64_to_i64(page.covered_through.sequence)?;
            let stamp = lexical
                .read_view()
                .map_err(|_| DomainError::new("lexical_index_failed", "Cannot pin lexical index."))?
                .stamp;
            if stamp.ack < through {
                let mut streams = Vec::new();
                for (unit, body) in units.iter().zip(&acquired) {
                    streams.extend(head_streams(unit, body)?);
                }
                lexical
                    .commit_stream(
                        through,
                        units.iter().map(|u| Ok(index_id(u))),
                        streams.into_iter().flatten(),
                    )
                    .map_err(|_| {
                        DomainError::new(
                            "lexical_index_failed",
                            "Cannot commit acquired lexical records.",
                        )
                    })?;
            }
            let tx = db(self.connection.transaction())?;
            for (unit, body) in units.iter().zip(&acquired) {
                apply_unit(&tx, unit, body)?;
            }
            db(tx.execute("INSERT INTO exact_position VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET epoch=excluded.epoch,sequence=excluded.sequence",params![page.covered_through.epoch,page.covered_through.sequence.to_string()]))?;
            db(tx.commit())?;
            cursor = page.covered_through;
        }
        Ok(cursor)
    }
    pub fn lexical_search(
        &self,
        query: &str,
        context: &RequestContext,
        limit: usize,
    ) -> Result<Vec<agentlaw_search::Hit>> {
        self.search_scopes(query, &allowed_scopes(context), limit)
    }
    pub fn all_scopes(&self) -> Result<Vec<String>> {
        let mut st = db(self
            .connection
            .prepare("SELECT DISTINCT scope FROM exact_heads ORDER BY scope"))?;
        let rows = db(st.query_map([], |r| r.get(0)))?;
        db(rows.collect())
    }
    pub fn search_scopes(
        &self,
        query: &str,
        scopes: &[String],
        limit: usize,
    ) -> Result<Vec<agentlaw_search::Hit>> {
        let position = self.position()?.ok_or_else(|| {
            DomainError::new(
                "exact_index_coverage_unknown",
                "No verified source position is indexed.",
            )
        })?;
        let lexical = self.lexical(&position.epoch)?;
        let mut view = lexical.read_view().map_err(|_| {
            DomainError::new("lexical_index_failed", "Cannot pin lexical search view.")
        })?;
        if view.stamp.ack < u64_to_i64(position.sequence)? {
            return Err(DomainError::new(
                "lexical_index_incomplete",
                "Lexical index has not committed the required source position.",
            ));
        }
        view.search(
            query,
            &agentlaw_search::ScopeFilter {
                allowed_scopes: scopes.to_vec(),
            },
            limit,
            &[],
            &[],
            true,
        )
        .map_err(|_| DomainError::new("search_failed", "Indexed lexical search failed."))
    }
    pub fn rebuild(
        &mut self,
        source: &agentlaw_storage::Store,
        control: &RequestControl,
    ) -> Result<SourcePosition> {
        let maintenance = self.maintenance_gate()?;
        control.phase("rebuilding_exact_metadata");
        let reader = source.owned_published_reader();
        let before = reader.position().map_err(|_| {
            DomainError::new(
                "source_unavailable",
                "Cannot inspect source before rebuild.",
            )
        })?;
        let filename = format!("lexical-rebuild-{}.sqlite", uuid::Uuid::new_v4());
        let mut lexical = agentlaw_search::SearchIndex::open(self.directory.join(&filename))
            .map_err(|_| {
                DomainError::new(
                    "lexical_index_failed",
                    "Cannot prepare a private rebuild index.",
                )
            })?;
        let tx = db(self.connection.transaction())?;
        db(tx.execute_batch("DELETE FROM exact_units;DELETE FROM exact_heads;DELETE FROM exact_edges;DELETE FROM exact_targets;DELETE FROM exact_bodies;DELETE FROM exact_previews;"))?;
        let mut cursor = None;
        loop {
            control.check()?;
            let (_, ids, more) = reader.inventory_page(cursor.as_deref(), 128).map_err(|_| {
                DomainError::new(
                    "exact_index_rebuild_failed",
                    "Cannot enumerate canonical identities.",
                )
            })?;
            for (kind, id) in ids {
                control.check()?;
                let acquired = if kind == "memory" {
                    reader.acquire_current(&id)
                } else {
                    reader.acquire_procedure(&id)
                }
                .map_err(|_| {
                    DomainError::new(
                        "exact_index_rebuild_failed",
                        "Cannot acquire canonical identity.",
                    )
                })?;
                let unit = acquired_unit(&acquired)?;
                apply_unit(&tx, &unit, &acquired)?;
                lexical
                    .commit_stream(
                        0,
                        std::iter::once(Ok(index_id(&unit))),
                        head_streams(&unit, &acquired)?.into_iter().flatten(),
                    )
                    .map_err(|_| {
                        DomainError::new(
                            "lexical_index_failed",
                            "Cannot populate private rebuild index.",
                        )
                    })?;
                cursor = Some(format!("{kind}/{id}"));
            }
            if !more {
                break;
            }
        }
        lexical
            .commit(u64_to_i64(before.sequence)?, &[], &[])
            .map_err(|_| {
                DomainError::new("lexical_index_failed", "Cannot seal private rebuild index.")
            })?;
        db(tx.execute("INSERT INTO exact_lexical VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET filename=excluded.filename",[filename]))?;
        db(tx.execute("INSERT INTO exact_position VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET epoch=excluded.epoch,sequence=excluded.sequence",params![before.epoch,before.sequence.to_string()]))?;
        db(tx.commit())?;
        drop(maintenance);
        self.synchronize(source, control)
    }
    pub fn ids_matching(
        &self,
        context: &RequestContext,
        rules: bool,
        tasks: bool,
    ) -> Result<Vec<String>> {
        let scopes = allowed_scopes(context);
        let mut result = BTreeSet::new();
        for scope in scopes {
            let mut st=db(self.connection.prepare("SELECT DISTINCT id FROM exact_heads WHERE kind='memory' AND scope=?1 AND ((?2=1 AND is_rule=1) OR (?3=1 AND task=1)) ORDER BY id"))?;
            let rows = db(st.query_map(params![scope, rules, tasks], |r| r.get::<_, String>(0)))?;
            result.extend(db(rows.collect::<std::result::Result<Vec<_>, _>>())?);
        }
        Ok(result.into_iter().collect())
    }
    pub fn reverse_required(&self, targets: &[String]) -> Result<Vec<String>> {
        let mut result = BTreeSet::new();
        let mut st=db(self.connection.prepare("SELECT DISTINCT source FROM exact_edges WHERE target=?1 AND kind='required' ORDER BY source"))?;
        for target in targets {
            let rows = db(st.query_map([target], |r| r.get::<_, String>(0)))?;
            result.extend(db(rows.collect::<std::result::Result<Vec<_>, _>>())?);
        }
        Ok(result.into_iter().collect())
    }
    pub fn related(&self, ids: &[String]) -> Result<Vec<(String, String)>> {
        let mut result = BTreeSet::new();
        let mut st=db(self.connection.prepare("SELECT DISTINCT target FROM exact_edges WHERE source=?1 AND kind='related' ORDER BY target"))?;
        for id in ids {
            for target in db(st.query_map([id], |r| r.get::<_, String>(0)))? {
                result.insert((id.clone(), db(target)?));
            }
        }
        Ok(result.into_iter().collect())
    }
    pub fn work_targets(
        &self,
        project: &str,
        requested: &[WorkTarget],
    ) -> Result<Vec<(String, Reading)>> {
        let mut result = BTreeSet::new();
        for target in requested {
            let path = relative_path(&target.path)?;
            let escaped = path
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            let mut st=db(self.connection.prepare("SELECT DISTINCT source,reading FROM exact_targets WHERE project=?1 AND (path=?2 OR (?3=1 AND path LIKE ?4 ESCAPE '\\')) ORDER BY source"))?;
            let rows = db(st.query_map(
                params![
                    project,
                    path,
                    target.kind == TargetKind::Directory,
                    format!("{escaped}/%")
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            ))?;
            result.extend(db(rows.collect::<std::result::Result<Vec<_>, _>>())?);
        }
        result
            .into_iter()
            .map(|(id, reading)| {
                Ok((
                    id,
                    match reading.as_str() {
                        "required" => Reading::Required,
                        "related" => Reading::Related,
                        _ => {
                            return Err(DomainError::new(
                                "exact_index_corrupt",
                                "Unknown indexed reading relation.",
                            ))
                        }
                    },
                ))
            })
            .collect()
    }
    pub fn heads(&self, ids: &[String]) -> Result<Vec<HeadRecord>> {
        let mut result = Vec::new();
        let mut st = db(self.connection.prepare(
            "SELECT h.id,h.kind,h.version,h.metadata,p.excerpt FROM exact_heads h JOIN exact_previews p USING(id,version) WHERE h.id=?1 ORDER BY h.version",
        ))?;
        for id in ids {
            let rows = db(st.query_map([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            }))?;
            for row in rows {
                let (id, kind, version, metadata, excerpt) = db(row)?;
                result.push(HeadRecord {
                    entity_id: id,
                    entity_type: kind,
                    observed_version: version,
                    excerpt,
                    metadata: serde_json::from_str(&metadata).map_err(|_| {
                        DomainError::new(
                            "exact_index_corrupt",
                            "Indexed metadata cannot be decoded.",
                        )
                    })?,
                });
            }
        }
        Ok(result)
    }
    pub fn procedure_ids(&self, context: &RequestContext) -> Result<Vec<String>> {
        let mut result = BTreeSet::new();
        for scope in allowed_scopes(context) {
            let mut st=db(self.connection.prepare("SELECT DISTINCT id FROM exact_heads WHERE kind='learned_procedure' AND scope=?1 ORDER BY id"))?;
            result.extend(db(db(st.query_map([scope], |r| r.get::<_, String>(0)))?
                .collect::<std::result::Result<Vec<_>, _>>())?);
        }
        Ok(result.into_iter().collect())
    }
}
pub fn allowed_scopes(context: &RequestContext) -> Vec<String> {
    let mut scopes = vec!["user".into(), format!("machine:{}", context.machine_id)];
    if let Some(project) = &context.project_id {
        scopes.push(format!("project:{project}"));
        scopes.push(format!("project:{project}:machine:{}", context.machine_id));
    }
    scopes
}
fn index_id(unit: &CurrentUnit) -> String {
    if unit.entity_type == "learned_procedure" {
        format!("procedure/{}", unit.entity_id)
    } else {
        unit.entity_id.clone()
    }
}
fn acquired_unit(acquired: &agentlaw_storage::acquire::AcquiredCurrent) -> Result<CurrentUnit> {
    let v = &acquired.state;
    let procedure = v["entity_type"] == "learned_procedure";
    let id = v[if procedure {
        "procedure_id"
    } else {
        "memory_id"
    }]
    .as_str()
    .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired ID."))?;
    let state = if v["state"] == "redirect" {
        UnitState::Redirect {
            redirect_to: v["redirect_to"]
                .as_str()
                .ok_or_else(|| DomainError::new("source_corrupt", "Missing redirect target."))?
                .into(),
            consolidation_change_id: v["consolidation_change_id"]
                .as_str()
                .ok_or_else(|| DomainError::new("source_corrupt", "Missing redirect change."))?
                .into(),
        }
    } else {
        UnitState::Live {
            heads: v["current_heads"]
                .as_array()
                .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired heads."))?
                .iter()
                .map(|metadata| agentlaw_storage::Head {
                    metadata: metadata.clone(),
                    body: String::new(),
                })
                .collect(),
        }
    };
    Ok(CurrentUnit {
        entity_id: id.into(),
        entity_type: if procedure {
            "learned_procedure"
        } else {
            "memory"
        }
        .into(),
        state,
    })
}
struct HeadStream {
    template: agentlaw_search::SearchDocument,
    reader: Option<std::fs::File>,
    pending: Vec<u8>,
    done: bool,
    emitted: bool,
}
impl Iterator for HeadStream {
    type Item = agentlaw_search::Result<agentlaw_search::SearchDocument>;
    fn next(&mut self) -> Option<Self::Item> {
        use std::io::Read;
        if self.done {
            return None;
        }
        let result = (|| {
            if let Some(reader) = &mut self.reader {
                let mut buf = [0u8; 65536];
                let n = reader.read(&mut buf).map_err(|_| {
                    agentlaw_search::Error::Invalid("acquired body read failed".into())
                })?;
                self.pending.extend_from_slice(&buf[..n]);
                let valid = match std::str::from_utf8(&self.pending) {
                    Ok(s) => s.len(),
                    Err(e) if e.error_len().is_none() && n != 0 => e.valid_up_to(),
                    Err(_) => {
                        return Err(agentlaw_search::Error::Invalid("body UTF-8 failed".into()))
                    }
                };
                if n == 0 && self.pending.is_empty() && self.emitted {
                    self.done = true;
                    return Ok(None);
                }
                let body = String::from_utf8(self.pending.drain(..valid).collect())
                    .map_err(|_| agentlaw_search::Error::Invalid("body UTF-8 failed".into()))?;
                self.emitted = true;
                if n == 0 {
                    self.done = true
                }
                Ok(Some(agentlaw_search::SearchDocument {
                    body,
                    ..self.template.clone()
                }))
            } else {
                self.done = true;
                Ok(Some(self.template.clone()))
            }
        })();
        match result {
            Ok(value) => value.map(Ok),
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}
fn head_streams(
    unit: &CurrentUnit,
    acquired: &agentlaw_storage::acquire::AcquiredCurrent,
) -> Result<Vec<HeadStream>> {
    let mut streams = Vec::new();
    for (index, head) in unit.heads().iter().enumerate() {
        let change = &acquired
            .references
            .get(index)
            .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired version."))?
            .observed_version;
        let reader = if unit.entity_type == "memory" {
            Some(
                acquired
                    .bodies
                    .get(head.metadata["change_id"].as_str().unwrap_or(""))
                    .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired body."))?
                    .open()
                    .map_err(|_| {
                        DomainError::new("source_unavailable", "Cannot open acquired body.")
                    })?,
            )
        } else {
            None
        };
        streams.push(HeadStream {
            template: agentlaw_search::SearchDocument {
                memory_id: index_id(unit),
                change_id: change.clone(),
                body: index_body(unit, head),
                scope: index_scope(unit, head)?,
            },
            reader,
            pending: Vec::with_capacity(65540),
            done: false,
            emitted: false,
        });
    }
    Ok(streams)
}
fn index_body(unit: &CurrentUnit, head: &agentlaw_storage::Head) -> String {
    if unit.entity_type == "learned_procedure" {
        format!(
            "{}\n{}",
            head.metadata["name"].as_str().unwrap_or(""),
            head.metadata["use_when"].as_str().unwrap_or("")
        )
    } else {
        head.body.clone()
    }
}
fn index_scope(unit: &CurrentUnit, head: &agentlaw_storage::Head) -> Result<String> {
    let scope = crate::runtime::scope_token(&crate::runtime::scope_value(
        &head.metadata["applicability"],
    )?);
    Ok(if unit.entity_type == "learned_procedure" {
        format!("procedure:{scope}")
    } else {
        scope
    })
}
fn apply_unit(
    tx: &rusqlite::Transaction<'_>,
    unit: &CurrentUnit,
    acquired: &agentlaw_storage::acquire::AcquiredCurrent,
) -> Result<()> {
    for table in [
        "exact_heads",
        "exact_edges",
        "exact_targets",
        "exact_bodies",
        "exact_previews",
    ] {
        let key = if table == "exact_heads" || table == "exact_bodies" || table == "exact_previews"
        {
            "id"
        } else {
            "source"
        };
        db(tx.execute(
            &format!("DELETE FROM {table} WHERE {key}=?1"),
            [&unit.entity_id],
        ))?;
    }
    let redirect = match &unit.state {
        UnitState::Redirect { redirect_to, .. } => Some(redirect_to),
        _ => None,
    };
    db(tx.execute("INSERT INTO exact_units VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind,redirect=excluded.redirect",params![unit.entity_id,unit.entity_type,redirect]))?;
    for (index, head) in unit.heads().iter().enumerate() {
        let version = &acquired
            .references
            .get(index)
            .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired version."))?
            .observed_version;
        db(tx.execute(
            "INSERT INTO exact_bodies VALUES(?1,?2,?3)",
            params![
                unit.entity_id,
                version,
                acquired
                    .bodies
                    .get(head.metadata["change_id"].as_str().unwrap_or(""))
                    .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired body."))?
                    .sha256
            ],
        ))?;
        let applicability = crate::runtime::scope_value(&head.metadata["applicability"])?;
        {
            use std::io::Read;
            let mut prefix = Vec::with_capacity(1600);
            acquired
                .bodies
                .get(head.metadata["change_id"].as_str().unwrap_or(""))
                .ok_or_else(|| DomainError::new("source_corrupt", "Missing acquired body."))?
                .open()
                .map_err(|_| DomainError::new("source_unavailable", "Cannot read preview."))?
                .take(1600)
                .read_to_end(&mut prefix)
                .map_err(|_| DomainError::new("source_unavailable", "Cannot read preview."))?;
            let text = match std::str::from_utf8(&prefix) {
                Ok(text) => text,
                Err(e) if e.error_len().is_none() => {
                    std::str::from_utf8(&prefix[..e.valid_up_to()])
                        .map_err(|_| DomainError::new("source_corrupt", "Invalid body UTF-8."))?
                }
                Err(_) => return Err(DomainError::new("source_corrupt", "Invalid body UTF-8.")),
            };
            let excerpt: String = text.chars().take(400).collect();
            db(tx.execute(
                "INSERT INTO exact_previews VALUES(?1,?2,?3)",
                params![unit.entity_id, version, excerpt],
            ))?;
        }
        let scope = crate::runtime::scope_token(&applicability);
        let is_rule = head.metadata.get("is_rule").and_then(Value::as_bool);
        let task = head.metadata.get("in_working_set").and_then(Value::as_bool);
        db(tx.execute(
            "INSERT INTO exact_heads VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                unit.entity_id,
                version,
                unit.entity_type,
                scope,
                is_rule,
                task,
                head.metadata.to_string()
            ],
        ))?;
        if unit.entity_type == "memory" {
            for relation in head.metadata["relations"].as_array().ok_or_else(|| {
                DomainError::new(
                    "source_corrupt",
                    "Published memory relation metadata is missing.",
                )
            })? {
                let target = relation["target_memory_id"].as_str().ok_or_else(|| {
                    DomainError::new("source_corrupt", "Published relation target is missing.")
                })?;
                let kind = relation["kind"].as_str().ok_or_else(|| {
                    DomainError::new("source_corrupt", "Published relation kind is missing.")
                })?;
                db(tx.execute(
                    "INSERT INTO exact_edges VALUES(?1,?2,?3,?4)",
                    params![unit.entity_id, version, target, kind],
                ))?;
            }
            for target in head.metadata["work_targets"].as_array().ok_or_else(|| {
                DomainError::new(
                    "source_corrupt",
                    "Published work-target metadata is missing.",
                )
            })? {
                let target: StoredTarget =
                    serde_json::from_value(target.clone()).map_err(|_| {
                        DomainError::new("source_corrupt", "Published work target is invalid.")
                    })?;
                db(tx.execute(
                    "INSERT INTO exact_targets VALUES(?1,?2,?3,?4,?5,?6)",
                    params![
                        unit.entity_id,
                        version,
                        target.project_id,
                        relative_path(&target.path)?,
                        match target.kind {
                            TargetKind::File => "file",
                            TargetKind::Directory => "directory",
                        },
                        match target.reading {
                            Reading::Required => "required",
                            Reading::Related => "related",
                        }
                    ],
                ))?;
            }
        }
    }
    Ok(())
}
