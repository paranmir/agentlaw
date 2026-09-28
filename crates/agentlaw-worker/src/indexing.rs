//! Durable channel-specific indexing handoff. Source acceptance and index acknowledgement differ.
use crate::{
    derived::{DerivedContext, SourcePosition},
    Broker,
};
use agentlaw_search::{ExactVectorIndex, SearchDocument, SearchIndex, VectorRecord, ViewStamp};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Channel {
    Lexical,
    Vector,
}
impl Channel {
    pub fn name(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Vector => "vector",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexDocument {
    pub memory_id: String,
    pub change_id: String,
    pub section: String,
    pub scope: String,
    pub body: Option<String>,
    pub vector: Option<Vec<f32>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexBatch {
    pub token: String,
    pub channel: Channel,
    pub basis: SourcePosition,
    pub covered_through: SourcePosition,
    pub documents: Vec<IndexDocument>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexReceipt {
    pub token: String,
    pub channel: Channel,
    pub generation_id: String,
    pub index_commit_id: i64,
    pub source_position: SourcePosition,
}
pub(crate) fn initialize(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS derived_publications(repository TEXT NOT NULL,model TEXT NOT NULL,config TEXT NOT NULL,epoch TEXT NOT NULL,sequence INTEGER NOT NULL,PRIMARY KEY(repository,model,config,sequence));
CREATE TABLE IF NOT EXISTS derived_members(repository TEXT NOT NULL,model TEXT NOT NULL,config TEXT NOT NULL,sequence INTEGER NOT NULL,job INTEGER NOT NULL,PRIMARY KEY(repository,model,config,sequence,job));
CREATE TABLE IF NOT EXISTS generation_ack(repository TEXT NOT NULL,model TEXT NOT NULL,config TEXT NOT NULL,channel TEXT NOT NULL,epoch TEXT NOT NULL,sequence INTEGER NOT NULL,generation_id TEXT NOT NULL,index_commit_id INTEGER NOT NULL,PRIMARY KEY(repository,model,config,channel));
CREATE TABLE IF NOT EXISTS index_offers(token TEXT PRIMARY KEY,repository TEXT NOT NULL,model TEXT NOT NULL,config TEXT NOT NULL,channel TEXT NOT NULL,epoch TEXT NOT NULL,basis INTEGER NOT NULL,through_sequence INTEGER NOT NULL);")
}
fn key(c: &DerivedContext) -> [&str; 3] {
    [&c.repository_id, &c.model_digest, &c.config_digest]
}
pub fn bootstrap_channel_complete(
    b: &Broker,
    c: &DerivedContext,
    channel: Channel,
) -> Result<bool> {
    let exists:bool=b.connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='derived_bootstrap')",[],|r|r.get(0))?;
    if !exists {
        return Ok(true);
    }
    Ok(b.connection.query_row(&format!("SELECT complete AND {} FROM derived_bootstrap WHERE repository=?1 AND model=?2 AND config=?3",channel.name()),key(c),|r|r.get(0)).optional()?.unwrap_or(true))
}
pub fn channel_position(
    b: &Broker,
    c: &DerivedContext,
    channel: Channel,
) -> Result<SourcePosition> {
    initialize(&b.connection)?;
    Ok(b.connection.query_row("SELECT epoch,sequence FROM generation_ack WHERE repository=?1 AND model=?2 AND config=?3 AND channel=?4",params![c.repository_id,c.model_digest,c.config_digest,channel.name()],|r|Ok(SourcePosition{epoch:r.get(0)?,sequence:r.get::<_,i64>(1)? as u64})).optional()?.unwrap_or(c.initial_basis.clone()))
}
pub fn ready_index_batch(
    b: &mut Broker,
    c: &DerivedContext,
    channel: Channel,
    max_records: usize,
) -> Result<Option<IndexBatch>> {
    initialize(&b.connection)?;
    if max_records == 0 || max_records > 65536 {
        return Err("invalid index batch bound".into());
    }
    if !bootstrap_channel_complete(b, c, channel)? {
        return Err("initial inventory handoff requires the streaming backend".into());
    }
    let basis = channel_position(b, c, channel)?;
    let through = ready_through(b, c, channel, &basis)?;
    if through == basis.sequence {
        return Ok(None);
    }
    let epoch = basis.epoch.clone();
    // Select ALL current-head records for the latest publication touching each ID.
    let mut st=b.connection.prepare("WITH latest AS(SELECT j.memory,MAX(m.sequence) seq FROM derived_members m JOIN jobs j ON j.id=m.job WHERE m.repository=?1 AND m.model=?2 AND m.config=?3 AND m.sequence>?4 AND m.sequence<=?5 GROUP BY j.memory) SELECT DISTINCT j.memory,j.change_id,j.section,d.scope,d.tombstone,j.payload,j.result FROM derived_members m JOIN jobs j ON j.id=m.job JOIN derived_documents d ON d.job=j.id JOIN latest l ON l.memory=j.memory AND l.seq=m.sequence WHERE m.repository=?1 AND m.model=?2 AND m.config=?3 ORDER BY j.memory,j.change_id,j.section")?;
    let rows = st.query_map(
        params![
            c.repository_id,
            c.model_digest,
            c.config_digest,
            basis.sequence as i64,
            through as i64
        ],
        |r| {
            let tombstone: bool = r.get(4)?;
            let blob: Option<Vec<u8>> = r.get(6)?;
            Ok(IndexDocument {
                memory_id: r.get(0)?,
                change_id: r.get(1)?,
                section: r.get(2)?,
                scope: r.get(3)?,
                body: if tombstone { None } else { Some(r.get(5)?) },
                vector: blob.map(|v| {
                    v.chunks_exact(4)
                        .map(|p| f32::from_le_bytes([p[0], p[1], p[2], p[3]]))
                        .collect()
                }),
            })
        },
    )?;
    let mut documents = Vec::new();
    let mut bytes = 0usize;
    for row in rows {
        if documents.len() >= max_records {
            return Err(
                "materialized handoff exceeds record budget; use streaming backend handoff".into(),
            );
        }
        let document = row?;
        let size = document.body.as_ref().map(|b| b.len()).unwrap_or(0)
            + document.vector.as_ref().map(|v| v.len() * 4).unwrap_or(0)
            + document.memory_id.len()
            + document.change_id.len()
            + document.section.len()
            + document.scope.len();
        bytes = bytes
            .checked_add(size)
            .ok_or("index handoff byte overflow")?;
        if bytes > 4 * 1024 * 1024 {
            return Err(
                "materialized handoff exceeds byte budget; use streaming backend handoff".into(),
            );
        }
        documents.push(document);
    }
    drop(st);
    let covered_through = SourcePosition {
        epoch,
        sequence: through,
    };
    let token = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            c,
            channel,
            &basis,
            &covered_through,
            &documents
        ))?)
    );
    b.connection.execute(
        "INSERT OR IGNORE INTO index_offers VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            token,
            c.repository_id,
            c.model_digest,
            c.config_digest,
            channel.name(),
            basis.epoch,
            basis.sequence as i64,
            through as i64
        ],
    )?;
    Ok(Some(IndexBatch {
        token,
        channel,
        basis,
        covered_through,
        documents,
    }))
}
pub fn acknowledge_index(b: &mut Broker, c: &DerivedContext, receipt: &IndexReceipt) -> Result<()> {
    if receipt.source_position.epoch != c.initial_basis.epoch {
        return Err("index epoch mismatch".into());
    }
    let tx = b
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let offer:Option<(String,i64,i64)>=tx.query_row("SELECT epoch,basis,through_sequence FROM index_offers WHERE token=?1 AND repository=?2 AND model=?3 AND config=?4 AND channel=?5",params![receipt.token,c.repository_id,c.model_digest,c.config_digest,receipt.channel.name()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let Some((epoch, basis, through)) = offer else {
        return Err("index receipt has no matching durable offer".into());
    };
    if epoch != receipt.source_position.epoch
        || through as u64 != receipt.source_position.sequence
        || receipt.index_commit_id <= 0
        || receipt.generation_id.is_empty()
    {
        return Err("index receipt does not match immutable committed batch".into());
    }
    let old:Option<(i64,String,i64)>=tx.query_row("SELECT sequence,generation_id,index_commit_id FROM generation_ack WHERE repository=?1 AND model=?2 AND config=?3 AND channel=?4",params![c.repository_id,c.model_digest,c.config_digest,receipt.channel.name()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if let Some((seq, generation, commit)) = old {
        if seq == through
            && generation == receipt.generation_id
            && commit == receipt.index_commit_id
        {
            return Ok(());
        }
        if seq != basis || generation != receipt.generation_id || receipt.index_commit_id <= commit
        {
            return Err("stale index acknowledgement or changed generation".into());
        }
    }
    tx.execute("INSERT INTO generation_ack VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(repository,model,config,channel) DO UPDATE SET epoch=excluded.epoch,sequence=excluded.sequence,generation_id=excluded.generation_id,index_commit_id=excluded.index_commit_id",params![c.repository_id,c.model_digest,c.config_digest,receipt.channel.name(),epoch,through,receipt.generation_id,receipt.index_commit_id])?;
    // Reclaim queue admission only when both channels acknowledge every occurrence.
    tx.execute("UPDATE jobs SET state='acknowledged' WHERE durable=1 AND id IN(SELECT m.job FROM derived_members m WHERE m.repository=?1 AND m.model=?2 AND m.config=?3 GROUP BY m.job HAVING MAX(m.sequence)<=(SELECT MIN(sequence) FROM generation_ack WHERE repository=?1 AND model=?2 AND config=?3) AND (SELECT COUNT(*) FROM generation_ack WHERE repository=?1 AND model=?2 AND config=?3)=2)",key(c))?;
    tx.commit()?;
    Ok(())
}
pub fn verify_receipt(state: &Path, c: &DerivedContext, receipt: &IndexReceipt) -> Result<()> {
    let dir = directory(state, c)?;
    let stamp = match receipt.channel {
        Channel::Lexical => {
            let p = dir.join("lexical.sqlite");
            if !p.is_file() {
                return Err("lexical receipt backend missing".into());
            }
            SearchIndex::open(p)?.read_view()?.stamp
        }
        Channel::Vector => {
            let p = dir.join("vector.sqlite");
            if !p.is_file() {
                return Err("vector receipt backend missing".into());
            }
            ExactVectorIndex::open(p, &c.model_digest, &c.config_digest, 256)?
                .read_view()?
                .stamp
        }
    };
    if receipt.generation_id != format!("{}:{}", dir.display(), stamp.generation)
        || receipt.index_commit_id != stamp.commit
        || receipt.source_position.sequence != stamp.ack as u64
        || receipt.source_position.epoch != c.initial_basis.epoch
    {
        return Err("receipt does not identify the actual immutable backend commit".into());
    }
    Ok(())
}
/// A no-work flush is not proof of recovery: it can also mean incomplete
/// bootstrap or pending embeddings. Verify the durable source fence and the
/// actual immutable backend before retiring a vector failure diagnostic.
pub(crate) fn verify_vector_recovery(b: &Broker, c: &DerivedContext, state: &Path) -> Result<bool> {
    if !bootstrap_channel_complete(b, c, Channel::Vector)? {
        return Ok(false);
    }
    let accepted: Option<(String, i64)> = b.connection.query_row(
        "SELECT epoch,accepted_through FROM derived_cursor WHERE repository=?1 AND model=?2 AND config=?3",
        key(c),
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    let Some((epoch, accepted)) = accepted else {
        return Ok(false);
    };
    if epoch != c.initial_basis.epoch {
        return Err("vector source epoch changed; rebuild required".into());
    }
    let ack: Option<(String, i64, String, i64)> = b.connection.query_row(
        "SELECT epoch,sequence,generation_id,index_commit_id FROM generation_ack WHERE repository=?1 AND model=?2 AND config=?3 AND channel='vector'",
        key(c),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).optional()?;
    let Some((ack_epoch, sequence, generation_id, index_commit_id)) = ack else {
        return Ok(false);
    };
    if accepted < 0 || sequence < 0 || ack_epoch != epoch || sequence < accepted {
        return Ok(false);
    }
    // Foreground index writers share this lock. The verifier never creates a
    // missing backend and cannot mistake a partially published generation for
    // an acknowledged one.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root_directory(state, c)?.join("writer.lock"))?;
    use fs2::FileExt;
    lock.lock_exclusive()?;
    verify_receipt(
        state,
        c,
        &IndexReceipt {
            token: String::new(),
            channel: Channel::Vector,
            generation_id,
            index_commit_id,
            source_position: SourcePosition {
                epoch,
                sequence: sequence as u64,
            },
        },
    )?;
    Ok(true)
}
fn root_directory(state: &Path, c: &DerivedContext) -> Result<PathBuf> {
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            c.repository_id.as_str(),
            c.model_digest.as_str(),
            c.config_digest.as_str(),
            c.initial_basis.epoch.as_str()
        ))?)
    );
    let p = state.join("indices").join(digest);
    std::fs::create_dir_all(&p)?;
    Ok(p)
}
pub fn generation_lease(
    state: &Path,
    c: &DerivedContext,
) -> Result<Option<agentlaw_search::generations::GenerationLease>> {
    let root = root_directory(state, c)?.join("generations");
    if !root.join("catalog.sqlite").is_file() {
        return Ok(None);
    }
    agentlaw_search::generations::GenerationCatalog::open(root)?.acquire()
}
pub fn directory(state: &Path, c: &DerivedContext) -> Result<PathBuf> {
    Ok(generation_lease(state, c)?
        .map(|g| g.directory.clone())
        .unwrap_or(root_directory(state, c)?))
}
pub fn flush_channel_buffered(
    b: &mut Broker,
    c: &DerivedContext,
    state: &Path,
    channel: Channel,
) -> Result<bool> {
    use fs2::FileExt;
    let dir = directory(state, c)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(format!("{}.lock", channel.name())))?;
    lock.lock_exclusive()?;
    let Some(batch) = ready_index_batch(b, c, channel, 65536)? else {
        return Ok(false);
    };
    let removed: Vec<_> = batch
        .documents
        .iter()
        .filter(|d| d.body.is_none())
        .map(|d| d.memory_id.clone())
        .collect();
    let stamp: ViewStamp = match channel {
        Channel::Lexical => {
            let docs: Vec<_> = batch
                .documents
                .iter()
                .filter_map(|d| {
                    d.body.as_ref().map(|body| SearchDocument {
                        memory_id: d.memory_id.clone(),
                        change_id: d.change_id.clone(),
                        scope: d.scope.clone(),
                        body: body.clone(),
                    })
                })
                .collect();
            let mut index = SearchIndex::open(dir.join("lexical.sqlite"))?;
            let ack = index.read_view()?.stamp.ack;
            if ack != batch.basis.sequence as i64 && ack != batch.covered_through.sequence as i64 {
                return Err("lexical backend coverage lost; rebuild required".into());
            }
            if ack != batch.covered_through.sequence as i64 {
                index.commit(batch.covered_through.sequence as i64, &docs, &removed)?;
            }
            index.read_view()?.stamp
        }
        Channel::Vector => {
            let mut records = Vec::new();
            for d in &batch.documents {
                if d.body.is_some() {
                    records.push(VectorRecord {
                        memory_id: d.memory_id.clone(),
                        change_id: d.change_id.clone(),
                        section: d.section.clone(),
                        scope: d.scope.clone(),
                        vector: d.vector.clone().ok_or("ready vector missing")?,
                    });
                }
            }
            let mut index = ExactVectorIndex::open(
                dir.join("vector.sqlite"),
                &c.model_digest,
                &c.config_digest,
                256,
            )?;
            let ack = index.read_view()?.stamp.ack;
            if ack != batch.basis.sequence as i64 && ack != batch.covered_through.sequence as i64 {
                return Err("vector backend coverage lost; rebuild required".into());
            }
            if ack != batch.covered_through.sequence as i64 {
                index.commit(batch.covered_through.sequence as i64, &records, &removed)?;
            }
            index.read_view()?.stamp
        }
    };
    acknowledge_index(
        b,
        c,
        &IndexReceipt {
            token: batch.token,
            channel,
            generation_id: format!("{}:{}", dir.display(), stamp.generation),
            index_commit_id: stamp.commit,
            source_position: batch.covered_through,
        },
    )?;
    Ok(true)
}
fn ready_through(
    b: &Broker,
    c: &DerivedContext,
    channel: Channel,
    basis: &SourcePosition,
) -> Result<u64> {
    let accepted:Option<(String,i64)>=b.connection.query_row("SELECT epoch,accepted_through FROM derived_cursor WHERE repository=?1 AND model=?2 AND config=?3",key(c),|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((epoch, accepted)) = accepted else {
        return Ok(basis.sequence);
    };
    if epoch != basis.epoch {
        return Err("source epoch changed; rebuild required".into());
    }
    if accepted as u64 <= basis.sequence {
        return Ok(basis.sequence);
    }
    let count:i64=b.connection.query_row("SELECT COUNT(*) FROM derived_publications WHERE repository=?1 AND model=?2 AND config=?3 AND epoch=?4 AND sequence>?5 AND sequence<=?6",params![c.repository_id,c.model_digest,c.config_digest,basis.epoch,basis.sequence as i64,accepted],|r|r.get(0))?;
    if count != accepted - basis.sequence as i64 {
        return Err("durable publication coverage lost".into());
    }
    if channel == Channel::Vector {
        let(pending,failed):(i64,i64)=b.connection.query_row("WITH latest AS(SELECT j.memory,MAX(m.sequence) seq FROM derived_members m JOIN jobs j ON j.id=m.job WHERE m.repository=?1 AND m.model=?2 AND m.config=?3 AND m.sequence>?4 AND m.sequence<=?5 GROUP BY j.memory) SELECT COALESCE(SUM(CASE WHEN j.state IN('ready_to_index','acknowledged') THEN 0 ELSE 1 END),0),COALESCE(SUM(CASE WHEN j.state='failed' THEN 1 ELSE 0 END),0) FROM derived_members m JOIN jobs j ON j.id=m.job JOIN latest l ON l.memory=j.memory AND l.seq=m.sequence WHERE m.repository=?1 AND m.model=?2 AND m.config=?3",params![c.repository_id,c.model_digest,c.config_digest,basis.sequence as i64,accepted],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if failed > 0 {
            return Err("latest derived inference failed; vector channel incomplete".into());
        }
        if pending > 0 {
            return Ok(basis.sequence);
        }
    }
    Ok(accepted as u64)
}
const CURRENT_ROWS:&str="WITH latest AS(SELECT j.memory,MAX(m.sequence) seq FROM derived_members m JOIN jobs j ON j.id=m.job WHERE m.repository=?1 AND m.model=?2 AND m.config=?3 AND m.sequence>?4 AND m.sequence<=?5 GROUP BY j.memory) SELECT DISTINCT j.memory,j.change_id,j.section,d.scope,d.tombstone,j.payload,j.result FROM derived_members m JOIN jobs j ON j.id=m.job JOIN derived_documents d ON d.job=j.id JOIN latest l ON l.memory=j.memory AND l.seq=m.sequence WHERE m.repository=?1 AND m.model=?2 AND m.config=?3 ORDER BY j.memory,j.change_id,j.section";
fn write_range(
    b: &Broker,
    c: &DerivedContext,
    channel: Channel,
    dir: &Path,
    basis: i64,
    through: u64,
) -> Result<ViewStamp> {
    let mut changed=b.connection.prepare("SELECT DISTINCT j.memory FROM derived_members m JOIN jobs j ON j.id=m.job WHERE m.repository=?1 AND m.model=?2 AND m.config=?3 AND m.sequence>?4 AND m.sequence<=?5 ORDER BY j.memory")?;
    let ids = changed
        .query_map(
            params![
                c.repository_id,
                c.model_digest,
                c.config_digest,
                basis as i64,
                through as i64
            ],
            |r| r.get::<_, String>(0),
        )?
        .map(|r| r.map_err(agentlaw_search::Error::from));
    let mut st = b.connection.prepare(CURRENT_ROWS)?;
    let records = st.query_map(
        params![
            c.repository_id,
            c.model_digest,
            c.config_digest,
            basis as i64,
            through as i64
        ],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<Vec<u8>>>(6)?,
            ))
        },
    )?;
    match channel {
        Channel::Lexical => {
            let mut index = SearchIndex::open(dir.join("lexical.sqlite"))?;
            let ack = index.read_view()?.stamp.ack;
            if basis >= 0 && ack != basis && ack != through as i64 {
                return Err("lexical backend coverage lost; rebuild required".into());
            }
            if basis < 0 || ack != through as i64 {
                let docs = records.filter_map(|r| match r {
                    Ok((memory_id, change_id, _, scope, false, body, _)) => {
                        Some(Ok(SearchDocument {
                            memory_id,
                            change_id,
                            scope,
                            body,
                        }))
                    }
                    Ok(_) => None,
                    Err(e) => Some(Err(e.into())),
                });
                index.commit_stream(through as i64, ids, docs)?;
            }
            Ok(index.read_view()?.stamp)
        }
        Channel::Vector => {
            let mut index = ExactVectorIndex::open(
                dir.join("vector.sqlite"),
                &c.model_digest,
                &c.config_digest,
                256,
            )?;
            let ack = index.read_view()?.stamp.ack;
            if basis >= 0 && ack != basis && ack != through as i64 {
                return Err("vector backend coverage lost; rebuild required".into());
            }
            if basis < 0 || ack != through as i64 {
                let records = records.filter_map(|r| match r {
                    Ok((memory_id, change_id, section, scope, false, _, Some(bytes))) => {
                        Some(Ok(VectorRecord {
                            memory_id,
                            change_id,
                            section,
                            scope,
                            vector: bytes
                                .chunks_exact(4)
                                .map(|v| f32::from_le_bytes([v[0], v[1], v[2], v[3]]))
                                .collect(),
                        }))
                    }
                    Ok((_, _, _, _, true, _, _)) => None,
                    Ok(_) => Some(Err(agentlaw_search::Error::Invalid(
                        "ready vector missing".into(),
                    ))),
                    Err(e) => Some(Err(e.into())),
                });
                index.commit_stream(through as i64, ids, records)?;
            }
            Ok(index.read_view()?.stamp)
        }
    }
}
pub fn flush_channel(
    b: &mut Broker,
    c: &DerivedContext,
    state: &Path,
    channel: Channel,
) -> Result<bool> {
    use fs2::FileExt;
    initialize(&b.connection)?;
    let root = root_directory(state, c)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("writer.lock"))?;
    lock.lock_exclusive()?;
    let dir = directory(state, c)?;
    if !bootstrap_channel_complete(b, c, channel)? {
        let complete: bool = b.connection.query_row(
            "SELECT complete FROM derived_bootstrap WHERE repository=?1 AND model=?2 AND config=?3",
            key(c),
            |r| r.get(0),
        )?;
        if !complete {
            return Ok(false);
        }
        let mut bootstrap_through = c.initial_basis.sequence;
        if channel == Channel::Vector {
            bootstrap_through = ready_through(b, c, channel, &c.initial_basis)?;
            let pending:bool=b.connection.query_row("WITH latest AS(SELECT j.memory,MAX(m.sequence) seq FROM derived_members m JOIN jobs j ON j.id=m.job WHERE m.repository=?1 AND m.model=?2 AND m.config=?3 AND m.sequence<=?4 GROUP BY j.memory) SELECT EXISTS(SELECT 1 FROM derived_members m JOIN jobs j ON j.id=m.job JOIN latest l ON l.memory=j.memory AND l.seq=m.sequence WHERE m.repository=?1 AND m.model=?2 AND m.config=?3 AND j.state NOT IN('ready_to_index','acknowledged'))",params![c.repository_id,c.model_digest,c.config_digest,bootstrap_through as i64],|r|r.get(0))?;
            if pending {
                return Ok(false);
            }
        }
        let stamp = write_range(b, c, channel, &dir, -1, bootstrap_through)?;
        let tx = b
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO generation_ack VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(repository,model,config,channel) DO UPDATE SET epoch=excluded.epoch,sequence=excluded.sequence,generation_id=excluded.generation_id,index_commit_id=excluded.index_commit_id",params![c.repository_id,c.model_digest,c.config_digest,channel.name(),c.initial_basis.epoch,stamp.ack,format!("{}:{}",dir.display(),stamp.generation),stamp.commit])?;
        tx.execute(
            &format!(
                "UPDATE derived_bootstrap SET {}=1 WHERE repository=?1 AND model=?2 AND config=?3",
                channel.name()
            ),
            key(c),
        )?;
        tx.commit()?;
        return Ok(true);
    }
    let basis = channel_position(b, c, channel)?;
    let through = ready_through(b, c, channel, &basis)?;
    if through == basis.sequence {
        return Ok(false);
    }
    let token = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            "stream-v1",
            c,
            channel,
            &basis,
            through
        ))?)
    );
    b.connection.execute(
        "INSERT OR IGNORE INTO index_offers VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            token,
            c.repository_id,
            c.model_digest,
            c.config_digest,
            channel.name(),
            basis.epoch,
            basis.sequence as i64,
            through as i64
        ],
    )?;
    let stamp = write_range(b, c, channel, &dir, basis.sequence as i64, through)?;
    acknowledge_index(
        b,
        c,
        &IndexReceipt {
            token,
            channel,
            generation_id: format!("{}:{}", dir.display(), stamp.generation),
            index_commit_id: stamp.commit,
            source_position: SourcePosition {
                epoch: basis.epoch,
                sequence: through,
            },
        },
    )?;
    Ok(true)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepairReceipt {
    pub generation_id: String,
    pub lexical: SourcePosition,
    pub vector: SourcePosition,
}
/// Rebuild only derived backends from accepted durable publication payloads. The old
/// generation remains readable until the verified replacement is atomically active.
pub fn repair_index(b: &mut Broker, c: &DerivedContext, state: &Path) -> Result<RepairReceipt> {
    b.connection.execute("UPDATE jobs SET state='retry_wait',attempts=0,next_attempt=0,incarnation=NULL WHERE durable=1 AND state='failed' AND model=?1 AND config=?2",params![c.model_digest,format!("{}:repository:{}",c.config_digest,c.repository_id)])?;
    use fs2::FileExt;
    let root = root_directory(state, c)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("writer.lock"))?;
    lock.lock_exclusive()?;
    let has_bootstrap:bool=b.connection.query_row("SELECT EXISTS(SELECT 1 FROM derived_publications WHERE repository=?1 AND model=?2 AND config=?3 AND sequence=?4)",params![c.repository_id,c.model_digest,c.config_digest,c.initial_basis.sequence as i64],|r|r.get(0))?;
    if c.initial_basis.sequence != 0 && !has_bootstrap {
        return Err("repair requires complete durable coverage from epoch start".into());
    }
    let mut catalog =
        agentlaw_search::generations::GenerationCatalog::open(root.join("generations"))?;
    let build = catalog.begin(&c.model_digest, &c.config_digest)?;
    let mut stamps = Vec::new();
    for channel in [Channel::Lexical, Channel::Vector] {
        let mut basis = c.initial_basis.clone();
        if has_bootstrap {
            if !bootstrap_channel_complete(b, c, channel)? {
                return Err("bootstrap channel must finish before generation repair".into());
            }
            let through = ready_through(b, c, channel, &basis)?;
            stamps.push(write_range(b, c, channel, &build.directory, -1, through)?);
            continue;
        }
        loop {
            let through = ready_through(b, c, channel, &basis)?;
            let stamp = write_range(
                b,
                c,
                channel,
                &build.directory,
                basis.sequence as i64,
                through,
            )?;
            if through == basis.sequence {
                stamps.push(stamp);
                break;
            }
            basis.sequence = through;
        }
    }
    let id = build.id.clone();
    let dir = build.directory.clone();
    catalog.publish_channels(
        build,
        stamps[0].ack,
        stamps[1].ack,
        &stamps[0],
        Some(&stamps[1]),
    )?;
    let tx = b
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    for (channel, stamp) in [Channel::Lexical, Channel::Vector].into_iter().zip(&stamps) {
        tx.execute("INSERT INTO generation_ack VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(repository,model,config,channel) DO UPDATE SET epoch=excluded.epoch,sequence=excluded.sequence,generation_id=excluded.generation_id,index_commit_id=excluded.index_commit_id",params![c.repository_id,c.model_digest,c.config_digest,channel.name(),c.initial_basis.epoch,stamp.ack,format!("{}:{}",dir.display(),stamp.generation),stamp.commit])?;
    }
    tx.commit()?;
    Ok(RepairReceipt {
        generation_id: id,
        lexical: SourcePosition {
            epoch: c.initial_basis.epoch.clone(),
            sequence: stamps[0].ack as u64,
        },
        vector: SourcePosition {
            epoch: c.initial_basis.epoch.clone(),
            sequence: stamps[1].ack as u64,
        },
    })
}
