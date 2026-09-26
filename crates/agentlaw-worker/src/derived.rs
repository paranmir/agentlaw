//! C7 consumes only this read-only port. It has no source writer or raw-file reader.
use crate::Broker;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourcePosition {
    pub epoch: String,
    pub sequence: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DerivedDocument {
    pub memory_id: String,
    pub change_id: String,
    pub scope: String,
    pub body: Option<String>,
    pub section: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishedBatch {
    pub sequence: u64,
    pub documents: Vec<DerivedDocument>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishedPage {
    pub basis: SourcePosition,
    pub covered_through: SourcePosition,
    pub source_position: SourcePosition,
    pub batches: Vec<PublishedBatch>,
    pub coverage_complete: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DerivedContext {
    pub repository_id: String,
    pub initial_basis: SourcePosition,
    pub model_digest: String,
    pub config_digest: String,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("source coverage lost: {0}")]
    CoverageLost(String),
    #[error("source read unavailable: {0}")]
    SourceUnavailable(String),
    #[error("derived admission overload; source cursor unchanged")]
    Overload,
    #[error("invalid source page: {0}")]
    InvalidPage(String),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
pub trait PublishedSourcePort {
    fn read_published_changes(
        &self,
        repository_id: &str,
        basis: &SourcePosition,
        limit: u32,
    ) -> Result<PublishedPage>;
}
pub struct DerivedWorkCoordinator<'a, S: PublishedSourcePort> {
    pub(crate) broker: &'a mut Broker,
    source: &'a S,
}
#[derive(Clone, Debug)]
pub struct ReadyDerived {
    pub job_id: i64,
    pub memory_id: String,
    pub change_id: String,
    pub section: String,
    pub scope: String,
    pub tombstone: bool,
    pub vector: Option<Vec<f32>>,
}
impl<'a, S: PublishedSourcePort> DerivedWorkCoordinator<'a, S> {
    pub fn new(broker: &'a mut Broker, source: &'a S) -> Result<Self> {
        broker.connection.execute_batch("CREATE TABLE IF NOT EXISTS derived_cursor(repository TEXT NOT NULL,model TEXT NOT NULL,config TEXT NOT NULL,epoch TEXT NOT NULL,accepted_through INTEGER NOT NULL,PRIMARY KEY(repository,model,config));CREATE TABLE IF NOT EXISTS derived_documents(job INTEGER PRIMARY KEY,repository TEXT NOT NULL,scope TEXT NOT NULL,tombstone INTEGER NOT NULL);")?;
        crate::indexing::initialize(&broker.connection)?;
        broker.connection.execute_batch("CREATE TABLE IF NOT EXISTS derived_contexts(repository TEXT NOT NULL,model TEXT NOT NULL,config TEXT NOT NULL,json TEXT NOT NULL,PRIMARY KEY(repository,model,config));")?;
        broker.connection.execute_batch("CREATE TABLE IF NOT EXISTS derived_ingress(digest TEXT PRIMARY KEY,epoch TEXT NOT NULL,sequence INTEGER NOT NULL);")?;
        broker.connection.execute_batch("CREATE TABLE IF NOT EXISTS derived_bootstrap(repository TEXT NOT NULL,model TEXT NOT NULL,config TEXT NOT NULL,next_page INTEGER NOT NULL,complete INTEGER NOT NULL,lexical INTEGER NOT NULL DEFAULT 0,vector INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(repository,model,config));CREATE TABLE IF NOT EXISTS bootstrap_pages(repository TEXT NOT NULL,model TEXT NOT NULL,config TEXT NOT NULL,page INTEGER NOT NULL,digest TEXT NOT NULL,final INTEGER NOT NULL,PRIMARY KEY(repository,model,config,page));")?;
        Ok(Self { broker, source })
    }
    pub fn advance(&mut self, context: &DerivedContext, limit: u32) -> Result<SourcePosition> {
        self.advance_with_spools(context, limit, None)
    }
    pub fn advance_with_spools(
        &mut self,
        context: &DerivedContext,
        limit: u32,
        spools: Option<(&std::path::Path, &[crate::spool::SpoolBodyRef])>,
    ) -> Result<SourcePosition> {
        self.advance_inner(context, limit, spools, None)
    }
    pub fn bootstrap_with_spools(
        &mut self,
        context: &DerivedContext,
        page_number: u64,
        final_page: bool,
        state: &std::path::Path,
        refs: &[crate::spool::SpoolBodyRef],
    ) -> Result<SourcePosition> {
        self.advance_inner(
            context,
            1,
            Some((state, refs)),
            Some((page_number, final_page)),
        )
    }
    fn advance_inner(
        &mut self,
        context: &DerivedContext,
        limit: u32,
        spools: Option<(&std::path::Path, &[crate::spool::SpoolBodyRef])>,
        bootstrap: Option<(u64, bool)>,
    ) -> Result<SourcePosition> {
        if limit == 0 || limit > 128 {
            return Err(Error::InvalidPage("bounded page limit required".into()));
        }
        let basis = self.position(context)?;
        let page = self
            .source
            .read_published_changes(&context.repository_id, &basis, limit)?;
        let ingress = ingress_digest(context, &page, spools.map(|(_, refs)| refs).unwrap_or(&[]))?;
        let status:Option<(i64,bool)>=self.broker.connection.query_row("SELECT next_page,complete FROM derived_bootstrap WHERE repository=?1 AND model=?2 AND config=?3",params![context.repository_id,context.model_digest,context.config_digest],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((number, final_page)) = bootstrap {
            if number > i64::MAX as u64
                || basis != context.initial_basis
                || page.batches.len() != 1
                || page.batches[0].sequence != basis.sequence
                || page.covered_through != basis
                || page.source_position != basis
            {
                return Err(Error::InvalidPage(
                    "bootstrap must be one inventory page at the pinned initial fence".into(),
                ));
            }
            let receipt:Option<(String,bool)>=self.broker.connection.query_row("SELECT digest,final FROM bootstrap_pages WHERE repository=?1 AND model=?2 AND config=?3 AND page=?4",params![context.repository_id,context.model_digest,context.config_digest,number as i64],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((digest, was_final)) = receipt {
                if digest == ingress && was_final == final_page {
                    return Ok(basis);
                }
                return Err(Error::InvalidPage(
                    "bootstrap replay differs from durable page".into(),
                ));
            }
            let (next, complete) = status.unwrap_or((0, false));
            if complete || number != next as u64 {
                return Err(Error::CoverageLost(
                    "bootstrap page gap or already finalized".into(),
                ));
            }
            let existing:bool=self.broker.connection.query_row("SELECT EXISTS(SELECT 1 FROM derived_cursor WHERE repository=?1 AND model=?2 AND config=?3)",params![context.repository_id,context.model_digest,context.config_digest],|r|r.get(0))?;
            if status.is_none() && existing {
                return Err(Error::CoverageLost(
                    "cannot bootstrap an already accepted context".into(),
                ));
            }
        } else if status.is_some_and(|(_, complete)| !complete) {
            return Err(Error::CoverageLost(
                "initial inventory is not finalized".into(),
            ));
        }
        if !page.coverage_complete
            || page.basis != basis
            || page.covered_through.epoch != basis.epoch
            || page.source_position.epoch != basis.epoch
        {
            return Err(Error::CoverageLost(
                "epoch/basis/coverage mismatch; explicit rebuild required".into(),
            ));
        }
        if page.batches.len() > limit as usize {
            return Err(Error::InvalidPage("source exceeded page limit".into()));
        }
        if page.batches.is_empty() && page.source_position.sequence > basis.sequence {
            return Err(Error::CoverageLost(
                "source reports unpublished coverage gap without a change page".into(),
            ));
        }
        let mut sequence = basis.sequence;
        let mut records = 0usize;
        for batch in &page.batches {
            if bootstrap.is_none() {
                sequence = sequence
                    .checked_add(1)
                    .ok_or_else(|| Error::InvalidPage("source counter overflow".into()))?;
            }
            if batch.sequence != sequence {
                return Err(Error::CoverageLost("publication sequence gap".into()));
            }
            records = records
                .checked_add(batch.documents.len())
                .ok_or(Error::Overload)?;
        }
        if sequence != page.covered_through.sequence
            || sequence > page.source_position.sequence
            || sequence > i64::MAX as u64
        {
            return Err(Error::CoverageLost("invalid covered watermark".into()));
        }
        if records > 65536 {
            return Err(Error::Overload);
        }
        let tx = self
            .broker
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest:Option<(String,i64)>=tx.query_row("SELECT epoch,accepted_through FROM derived_cursor WHERE repository=?1 AND model=?2 AND config=?3",params![context.repository_id,context.model_digest,context.config_digest],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if latest
            .as_ref()
            .map(|(epoch, n)| epoch != &basis.epoch || *n as u64 != basis.sequence)
            .unwrap_or(false)
        {
            return Err(Error::CoverageLost(
                "coordinator cursor changed; retry source read".into(),
            ));
        }
        let namespaced_config = format!(
            "{}:repository:{}",
            context.config_digest, context.repository_id
        );
        let mut used_spools = 0usize;
        for (batch_index, batch) in page.batches.iter().enumerate() {
            tx.execute("INSERT OR IGNORE INTO derived_publications(repository,model,config,epoch,sequence) VALUES(?1,?2,?3,?4,?5)",params![context.repository_id,context.model_digest,context.config_digest,basis.epoch,batch.sequence as i64])?;
            for (document_index, document) in batch.documents.iter().enumerate() {
                let matching: Vec<_> = spools
                    .map(|(_, refs)| {
                        refs.iter()
                            .filter(|r| {
                                r.batch_index == batch_index && r.document_index == document_index
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if matching.len() > 1 {
                    return Err(Error::InvalidPage(
                        "duplicate spool document reference".into(),
                    ));
                }
                if let Some(reference) = matching.first() {
                    if document.body.as_deref() != Some("") {
                        return Err(Error::InvalidPage(
                            "spool requires empty live placeholder".into(),
                        ));
                    }
                    used_spools += 1;
                    let (state, _) = spools.unwrap();
                    let sections = crate::spool::Sections::open(state, &reference.body)
                        .map_err(|e| Error::SourceUnavailable(e.to_string()))?;
                    let mut any = false;
                    for section in sections {
                        let section =
                            section.map_err(|e| Error::SourceUnavailable(e.to_string()))?;
                        any = true;
                        let mut part = document.clone();
                        part.section = format!("{}:part:{:016}", document.section, section.index);
                        part.body = Some(section.text);
                        store_document(&tx, context, &namespaced_config, batch.sequence, &part)?;
                    }
                    if !any {
                        store_document(&tx, context, &namespaced_config, batch.sequence, document)?;
                    }
                } else {
                    store_document(&tx, context, &namespaced_config, batch.sequence, document)?;
                }
            }
        }
        if used_spools != spools.map(|(_, r)| r.len()).unwrap_or(0) {
            return Err(Error::InvalidPage(
                "spool reference outside published page".into(),
            ));
        }
        tx.execute("INSERT INTO derived_cursor VALUES(?1,?2,?3,?4,?5) ON CONFLICT(repository,model,config) DO UPDATE SET epoch=excluded.epoch,accepted_through=excluded.accepted_through",params![context.repository_id,context.model_digest,context.config_digest,basis.epoch,sequence as i64])?;
        let json = serde_json::to_string(context).map_err(|e| Error::InvalidPage(e.to_string()))?;
        tx.execute(
            "INSERT OR IGNORE INTO derived_contexts VALUES(?1,?2,?3,?4)",
            params![
                context.repository_id,
                context.model_digest,
                context.config_digest,
                json
            ],
        )?;
        tx.execute("UPDATE worker SET idle_since=NULL", [])?;
        if let Some((number, final_page)) = bootstrap {
            let next = number
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| Error::InvalidPage("bootstrap counter overflow".into()))?;
            tx.execute("INSERT INTO derived_bootstrap(repository,model,config,next_page,complete) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(repository,model,config) DO UPDATE SET next_page=excluded.next_page,complete=excluded.complete",params![context.repository_id,context.model_digest,context.config_digest,next as i64,final_page])?;
            tx.execute(
                "INSERT INTO bootstrap_pages VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    context.repository_id,
                    context.model_digest,
                    context.config_digest,
                    number as i64,
                    ingress,
                    final_page
                ],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO derived_ingress VALUES(?1,?2,?3)",
            params![
                ingress,
                page.covered_through.epoch,
                page.covered_through.sequence as i64
            ],
        )?;
        tx.commit()?;
        Ok(page.covered_through)
    }
    pub fn position(&self, context: &DerivedContext) -> Result<SourcePosition> {
        Ok(self.broker.connection.query_row("SELECT epoch,accepted_through FROM derived_cursor WHERE repository=?1 AND model=?2 AND config=?3",params![context.repository_id,context.model_digest,context.config_digest],|r|Ok(SourcePosition{epoch:r.get(0)?,sequence:r.get::<_,i64>(1)? as u64})).optional()?.unwrap_or(context.initial_basis.clone()))
    }
    pub fn ready(&self, context: &DerivedContext, limit: u32) -> Result<Vec<ReadyDerived>> {
        if limit > 256 {
            return Err(Error::InvalidPage("ready page limit exceeded".into()));
        }
        let mut st=self.broker.connection.prepare("SELECT j.id,j.memory,j.change_id,j.section,d.scope,d.tombstone,j.result FROM jobs j JOIN derived_documents d ON d.job=j.id WHERE d.repository=?1 AND j.state='ready_to_index' AND j.model=?3 AND j.config=?4 ORDER BY j.id LIMIT ?2")?;
        let rows = st.query_map(
            params![
                context.repository_id,
                limit,
                context.model_digest,
                format!(
                    "{}:repository:{}",
                    context.config_digest, context.repository_id
                )
            ],
            |r| {
                let blob: Option<Vec<u8>> = r.get(6)?;
                Ok(ReadyDerived {
                    job_id: r.get(0)?,
                    memory_id: r.get(1)?,
                    change_id: r.get(2)?,
                    section: r.get(3)?,
                    scope: r.get(4)?,
                    tombstone: r.get(5)?,
                    vector: blob.map(|b| {
                        b.chunks_exact(4)
                            .map(|v| f32::from_le_bytes([v[0], v[1], v[2], v[3]]))
                            .collect()
                    }),
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}
fn ingress_digest(
    context: &DerivedContext,
    page: &PublishedPage,
    spools: &[crate::spool::SpoolBodyRef],
) -> Result<String> {
    let bytes = serde_json::to_vec(&(context, page, spools))
        .map_err(|e| Error::InvalidPage(e.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
pub fn accepted_receipt(
    b: &Broker,
    context: &DerivedContext,
    page: &PublishedPage,
    spools: &[crate::spool::SpoolBodyRef],
) -> Result<Option<SourcePosition>> {
    let digest = ingress_digest(context, page, spools)?;
    Ok(b.connection
        .query_row(
            "SELECT epoch,sequence FROM derived_ingress WHERE digest=?1",
            [digest],
            |r| {
                Ok(SourcePosition {
                    epoch: r.get(0)?,
                    sequence: r.get::<_, i64>(1)? as u64,
                })
            },
        )
        .optional()?)
}
pub fn bootstrap_receipt(
    b: &Broker,
    context: &DerivedContext,
    number: u64,
    final_page: bool,
    page: &PublishedPage,
    spools: &[crate::spool::SpoolBodyRef],
) -> Result<bool> {
    let number = i64::try_from(number)
        .map_err(|_| Error::InvalidPage("bootstrap counter overflow".into()))?;
    let prior:Option<(String,bool)>=b.connection.query_row("SELECT digest,final FROM bootstrap_pages WHERE repository=?1 AND model=?2 AND config=?3 AND page=?4",params![context.repository_id,context.model_digest,context.config_digest,number],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    match prior {
        None => Ok(false),
        Some((digest, last))
            if last == final_page && digest == ingress_digest(context, page, spools)? =>
        {
            Ok(true)
        }
        Some(_) => Err(Error::InvalidPage(
            "bootstrap replay differs from durable page".into(),
        )),
    }
}
fn store_document(
    tx: &rusqlite::Transaction<'_>,
    context: &DerivedContext,
    config: &str,
    sequence: u64,
    document: &DerivedDocument,
) -> Result<()> {
    let payload = document.body.as_deref().unwrap_or("");
    if payload.len() > 4 * 1024 * 1024 {
        return Err(Error::Overload);
    }
    let digest = format!("{:x}", Sha256::digest(payload.as_bytes()));
    let existing:Option<i64>=tx.query_row("SELECT id FROM jobs WHERE model=?1 AND config=?2 AND memory=?3 AND change_id=?4 AND digest=?5 AND section=?6",params![context.model_digest,config,document.memory_id,document.change_id,digest,document.section],|r|r.get(0)).optional()?;
    let id = if let Some(id) = existing {
        id
    } else {
        tx.execute("INSERT INTO jobs(model,config,memory,change_id,digest,section,payload,durable,priority,state) VALUES(?1,?2,?3,?4,?5,?6,?7,1,0,?8)",params![context.model_digest,config,document.memory_id,document.change_id,digest,document.section,payload,if document.body.is_none(){"ready_to_index"}else{"queued"}])?;
        tx.last_insert_rowid()
    };
    tx.execute("INSERT INTO derived_documents VALUES(?1,?2,?3,?4) ON CONFLICT(job) DO UPDATE SET scope=excluded.scope,tombstone=excluded.tombstone",params![id,context.repository_id,document.scope,document.body.is_none()])?;
    tx.execute(
        "INSERT OR IGNORE INTO derived_members VALUES(?1,?2,?3,?4,?5)",
        params![
            context.repository_id,
            context.model_digest,
            context.config_digest,
            sequence as i64,
            id
        ],
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Source {
        gap: bool,
        count: usize,
    }
    impl PublishedSourcePort for Source {
        fn read_published_changes(
            &self,
            _: &str,
            b: &SourcePosition,
            _: u32,
        ) -> Result<PublishedPage> {
            let sequence = b.sequence + if self.gap { 2 } else { 1 };
            Ok(PublishedPage {
                basis: b.clone(),
                covered_through: SourcePosition {
                    epoch: b.epoch.clone(),
                    sequence,
                },
                source_position: SourcePosition {
                    epoch: b.epoch.clone(),
                    sequence,
                },
                coverage_complete: true,
                batches: vec![PublishedBatch {
                    sequence,
                    documents: (0..self.count)
                        .map(|n| DerivedDocument {
                            memory_id: n.to_string(),
                            change_id: sequence.to_string(),
                            scope: "user".into(),
                            body: None,
                            section: "tombstone".into(),
                        })
                        .collect(),
                }],
            })
        }
    }
    fn context() -> DerivedContext {
        DerivedContext {
            repository_id: "repo".into(),
            initial_basis: SourcePosition {
                epoch: "epoch".into(),
                sequence: 0,
            },
            model_digest: "m".into(),
            config_digest: "c".into(),
        }
    }
    #[test]
    fn durable_cursor_and_tombstone_are_atomic() {
        let mut b = Broker::open(":memory:", 1).unwrap();
        b.connection.execute_batch("CREATE TRIGGER inject_disk_failure BEFORE INSERT ON jobs WHEN NEW.memory='1' BEGIN SELECT RAISE(ABORT,'injected storage failure'); END;").unwrap();
        let mut c = DerivedWorkCoordinator::new(
            &mut b,
            &Source {
                gap: false,
                count: 2,
            },
        )
        .unwrap();
        assert!(matches!(c.advance(&context(), 1), Err(Error::Sql(_))));
        drop(c);
        let count: i64 = b
            .connection
            .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        b.connection
            .execute_batch("DROP TRIGGER inject_disk_failure")
            .unwrap();
        let mut c = DerivedWorkCoordinator::new(
            &mut b,
            &Source {
                gap: false,
                count: 1,
            },
        )
        .unwrap();
        assert_eq!(c.advance(&context(), 1).unwrap().sequence, 1);
        assert!(c.ready(&context(), 10).unwrap()[0].tombstone);
    }
    #[test]
    fn gap_is_not_empty_success() {
        let mut b = Broker::open(":memory:", 4).unwrap();
        let mut c = DerivedWorkCoordinator::new(
            &mut b,
            &Source {
                gap: true,
                count: 0,
            },
        )
        .unwrap();
        assert!(matches!(
            c.advance(&context(), 1),
            Err(Error::CoverageLost(_))
        ));
    }
}
