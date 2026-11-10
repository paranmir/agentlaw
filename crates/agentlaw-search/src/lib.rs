//! Derived-only disk-backed search. No canonical source files are modified.
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};
use thiserror::Error;
mod ann;
pub mod generations;
pub mod vector_file;
pub use ann::AnnMetrics;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error("invalid search input: {0}")]
    Invalid(String),
    #[error("index coverage is unknown; rebuild required")]
    CoverageUnknown,
}
pub type Result<T> = std::result::Result<T, Error>;
pub type SearchResult<T> = Result<T>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchDocument {
    pub memory_id: String,
    pub change_id: String,
    pub body: String,
    pub scope: String,
}
#[derive(Clone, Debug, Default)]
pub struct ScopeFilter {
    pub allowed_scopes: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Hit {
    pub memory_id: String,
    pub change_id: String,
    pub score: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ViewStamp {
    pub generation: i64,
    pub commit: i64,
    pub ack: i64,
}
pub struct SearchIndex {
    connection: Connection,
    path: std::path::PathBuf,
}
pub struct ReadView {
    connection: Connection,
    pub stamp: ViewStamp,
}
pub struct LexicalDelta<'a> {
    pub pending: &'a [SearchDocument],
    pub removed: &'a [String],
    pub complete: bool,
}
pub struct EffectiveCorpus<'v> {
    view: &'v mut ReadView,
    n: f64,
    avg: f64,
}

fn configure(c: &Connection) -> Result<()> {
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    c.execute_batch("PRAGMA cache_size=-2048; PRAGMA temp_store=FILE;")?;
    Ok(())
}
fn tokens(text: &str) -> BTreeMap<String, i64> {
    let mut tokenizer = StreamTokenizer::default();
    tokenizer.push(text, true)
}
#[derive(Default)]
struct StreamTokenizer {
    hash: Sha256,
    word: bool,
}
impl StreamTokenizer {
    fn push(&mut self, text: &str, finish: bool) -> BTreeMap<String, i64> {
        let mut counts = BTreeMap::new();
        for c in text.chars() {
            if c.is_alphanumeric() || c == '_' {
                self.word = true;
                for lower in c.to_lowercase() {
                    let mut utf8 = [0; 4];
                    self.hash.update(lower.encode_utf8(&mut utf8).as_bytes());
                }
            } else if self.word {
                let term = format!("{:x}", std::mem::take(&mut self.hash).finalize());
                *counts.entry(term).or_insert(0) += 1;
                self.word = false;
            }
        }
        if finish && self.word {
            let term = format!("{:x}", std::mem::take(&mut self.hash).finalize());
            *counts.entry(term).or_insert(0) += 1;
            self.word = false;
        }
        counts
    }
}
impl SearchIndex {
    /// Open an existing lexical projection without creating or repairing schema.
    pub fn open_existing(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open_with_flags(
            path.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        if connection.is_readonly(rusqlite::DatabaseName::Main)? {
            return Err(Error::Sql(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_READONLY),
                None,
            )));
        }
        configure(&connection)?;
        let index = Self {
            connection,
            path: path.as_ref().to_path_buf(),
        };
        index.validate_existing()?;
        Ok(index)
    }

    /// Validate an existing projection without creating, repairing, or writing it.
    pub fn validate_existing(&self) -> Result<()> {
        let connection = &self.connection;
        let invalid =
            || Error::Invalid("lexical projection is incomplete; rebuild required".into());
        let classify = |error: rusqlite::Error| match &error {
            rusqlite::Error::SqliteFailure(code, message)
                if matches!(
                    code.code,
                    rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
                ) || message.as_deref().is_some_and(|s| {
                    s.contains("no such table") || s.contains("no such column")
                }) =>
            {
                invalid()
            }
            rusqlite::Error::QueryReturnedNoRows | rusqlite::Error::InvalidColumnType(..) => {
                invalid()
            }
            _ => Error::Sql(error),
        };
        for query in [
            "SELECT singleton,generation,commit_id,ack FROM metadata LIMIT 0",
            "SELECT id,memory,change_id,scope,len FROM docs LIMIT 0",
            "SELECT id,digest FROM lexicon LIMIT 0",
            "SELECT term,doc,tf FROM postings LIMIT 0",
            "SELECT singleton,version FROM analyzer LIMIT 0",
            "SELECT filter,n,total_len,heads FROM query_stats LIMIT 0",
            "SELECT filter,scope FROM query_stat_scopes LIMIT 0",
        ] {
            connection.prepare(query).map_err(&classify)?;
        }
        for name in ["postings_doc", "docs_scope"] {
            let count: i64 = connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    [name],
                    |row| row.get(0),
                )
                .map_err(&classify)?;
            if count != 1 {
                return Err(invalid());
            }
        }
        let (singleton, generation, commit, ack): (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT singleton,generation,commit_id,ack FROM metadata",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(&classify)?;
        let analyzer: i64 = connection
            .query_row(
                "SELECT version FROM analyzer WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(&classify)?;
        if singleton != 1 || generation < 0 || commit < 0 || ack < 0 || analyzer != 3 {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path.as_ref())?;
        configure(&connection)?;
        let old_schema:i64=connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='docs' AND sql LIKE '%memory TEXT UNIQUE%'",[],|r|r.get(0))?;
        if old_schema != 0 {
            return Err(Error::Invalid(
                "obsolete lexical schema; rebuild derived index".into(),
            ));
        }
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
          CREATE TABLE IF NOT EXISTS metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),generation INTEGER NOT NULL,commit_id INTEGER NOT NULL,ack INTEGER NOT NULL);
          INSERT OR IGNORE INTO metadata VALUES(1,1,0,0);
          CREATE TABLE IF NOT EXISTS docs(id INTEGER PRIMARY KEY,memory TEXT NOT NULL,change_id TEXT NOT NULL,scope TEXT NOT NULL,len INTEGER NOT NULL,UNIQUE(memory,change_id));
          CREATE TABLE IF NOT EXISTS lexicon(id INTEGER PRIMARY KEY,digest BLOB NOT NULL UNIQUE CHECK(length(digest)=32));
          CREATE TABLE IF NOT EXISTS postings(term INTEGER NOT NULL,doc INTEGER NOT NULL,tf INTEGER NOT NULL,PRIMARY KEY(term,doc)) WITHOUT ROWID;
          CREATE INDEX IF NOT EXISTS postings_doc ON postings(doc);")?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS analyzer(singleton INTEGER PRIMARY KEY,version INTEGER NOT NULL);INSERT OR IGNORE INTO analyzer SELECT 1,CASE WHEN EXISTS(SELECT 1 FROM docs) THEN 1 ELSE 3 END;")?;
        let analyzer: i64 =
            connection.query_row("SELECT version FROM analyzer", [], |r| r.get(0))?;
        if analyzer != 3 {
            return Err(Error::Invalid(
                "lexical analyzer changed; rebuild derived generation".into(),
            ));
        }
        connection.execute_batch("CREATE INDEX IF NOT EXISTS docs_scope ON docs(scope,memory);CREATE TABLE IF NOT EXISTS query_stats(filter TEXT PRIMARY KEY,n INTEGER NOT NULL,total_len INTEGER NOT NULL,heads INTEGER NOT NULL);CREATE TABLE IF NOT EXISTS query_stat_scopes(filter TEXT NOT NULL,scope TEXT NOT NULL,PRIMARY KEY(filter,scope));")?;
        Ok(Self {
            connection,
            path: path.as_ref().to_path_buf(),
        })
    }
    /// Records and acknowledgment are committed in the SAME durable SQLite backend transaction.
    /// Caller must supply every change through `source_generation` (including tombstones).
    pub fn commit(
        &mut self,
        source_generation: i64,
        documents: &[SearchDocument],
        removed: &[String],
    ) -> Result<()> {
        if documents.len() > 65536 {
            return Err(Error::Invalid("bounded ingestion batch exceeded".into()));
        }
        let tx = self.connection.transaction()?;
        let ack: i64 = tx.query_row("SELECT ack FROM metadata", [], |r| r.get(0))?;
        if source_generation < ack {
            return Err(Error::Invalid("watermark regression".into()));
        }
        tx.execute_batch(
            "DROP TABLE IF EXISTS temp.changed;CREATE TEMP TABLE changed(memory TEXT PRIMARY KEY);",
        )?;
        for id in removed
            .iter()
            .map(String::as_str)
            .chain(documents.iter().map(|d| d.memory_id.as_str()))
        {
            tx.execute("INSERT OR IGNORE INTO changed VALUES(?1)", [id])?;
        }
        adjust_stats(&tx, -1)?;
        for id in removed
            .iter()
            .map(String::as_str)
            .chain(documents.iter().map(|d| d.memory_id.as_str()))
        {
            tx.execute(
                "DELETE FROM postings WHERE doc IN (SELECT id FROM docs WHERE memory=?1)",
                [id],
            )?;
            tx.execute("DELETE FROM docs WHERE memory=?1", [id])?;
        }
        for d in documents {
            let terms = tokens(&d.body);
            let len: i64 = terms.values().sum();
            let id:i64=tx.query_row(
                "INSERT INTO docs(memory,change_id,scope,len) VALUES(?1,?2,?3,?4) ON CONFLICT(memory,change_id) DO UPDATE SET len=docs.len+excluded.len WHERE docs.scope=excluded.scope RETURNING id",
                params![d.memory_id, d.change_id, d.scope, len],
                |r|r.get(0),
            )?;
            for (term, tf) in terms {
                let term = intern_term(&tx, &term)?;
                tx.execute(
                    "INSERT INTO postings VALUES(?1,?2,?3) ON CONFLICT(term,doc) DO UPDATE SET tf=postings.tf+excluded.tf",
                    params![term, id, tf],
                )?;
            }
        }
        adjust_stats(&tx, 1)?;
        if tx.execute(
            "UPDATE metadata SET commit_id=commit_id+1,ack=?1 WHERE commit_id<9223372036854775807",
            [source_generation],
        )? != 1
        {
            return Err(Error::Invalid("index commit counter overflow".into()));
        }
        tx.commit()?;
        Ok(())
    }
    pub fn read_view(&self) -> Result<ReadView> {
        let c = Connection::open(&self.path)?;
        configure(&c)?;
        c.execute_batch("BEGIN DEFERRED;")?;
        let stamp = c.query_row("SELECT generation,commit_id,ack FROM metadata", [], |r| {
            Ok(ViewStamp {
                generation: r.get(0)?,
                commit: r.get(1)?,
                ack: r.get(2)?,
            })
        })?;
        Ok(ReadView {
            connection: c,
            stamp,
        })
    }
    /// Streams ordered head chunks in one atomic backend transaction. A lexical token
    /// may span arbitrary chunks: only its incremental SHA-256 state is retained.
    pub fn commit_stream(
        &mut self,
        source_generation: i64,
        changed_ids: impl Iterator<Item = Result<String>>,
        documents: impl Iterator<Item = Result<SearchDocument>>,
    ) -> Result<()> {
        let tx = self.connection.transaction()?;
        let ack: i64 = tx.query_row("SELECT ack FROM metadata", [], |r| r.get(0))?;
        if source_generation < ack {
            return Err(Error::Invalid("watermark regression".into()));
        }
        tx.execute_batch(
            "DROP TABLE IF EXISTS temp.changed;CREATE TEMP TABLE changed(memory TEXT PRIMARY KEY);",
        )?;
        for id in changed_ids {
            tx.execute("INSERT OR IGNORE INTO changed VALUES(?1)", [id?])?;
        }
        adjust_stats(&tx, -1)?;
        tx.execute("DELETE FROM postings WHERE doc IN(SELECT id FROM docs WHERE memory IN(SELECT memory FROM changed))",[])?;
        tx.execute(
            "DELETE FROM docs WHERE memory IN(SELECT memory FROM changed)",
            [],
        )?;
        let mut tokenizer = StreamTokenizer::default();
        let mut current: Option<(String, String, String, i64)> = None;
        for document in documents {
            let d = document?;
            let same = current.as_ref().is_some_and(|(m, c, s, _)| {
                m == &d.memory_id && c == &d.change_id && s == &d.scope
            });
            if !same {
                if let Some((_, _, _, id)) = current.take() {
                    store_terms(&tx, id, tokenizer.push("", true))?;
                }
                let id: i64 = tx.query_row(
                    "INSERT INTO docs(memory,change_id,scope,len) VALUES(?1,?2,?3,0) RETURNING id",
                    params![d.memory_id, d.change_id, d.scope],
                    |r| r.get(0),
                )?;
                current = Some((d.memory_id, d.change_id, d.scope, id));
            }
            store_terms(
                &tx,
                current.as_ref().unwrap().3,
                tokenizer.push(&d.body, false),
            )?;
        }
        if let Some((_, _, _, id)) = current {
            store_terms(&tx, id, tokenizer.push("", true))?;
        }
        adjust_stats(&tx, 1)?;
        if tx.execute(
            "UPDATE metadata SET ack=?1,commit_id=commit_id+1 WHERE commit_id<9223372036854775807",
            [source_generation],
        )? != 1
        {
            return Err(Error::Invalid("index counter overflow".into()));
        }
        tx.commit()?;
        Ok(())
    }
    /// At most 128 scope profiles are cached. First use counts metadata once; later
    /// publication commits adjust only the changed IDs in the same transaction.
    pub fn prepare_scope(&mut self, scope: &ScopeFilter) -> Result<()> {
        let key = scope_key(scope)?;
        let tx = self.connection.transaction()?;
        let exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM query_stats WHERE filter=?1",
            [&key],
            |r| r.get(0),
        )?;
        if exists == 0 {
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM query_stats", [], |r| r.get(0))?;
            if count >= 128 {
                tx.execute("DELETE FROM query_stat_scopes WHERE filter=(SELECT MIN(filter) FROM query_stats)",[])?;
                tx.execute(
                    "DELETE FROM query_stats WHERE filter=(SELECT MIN(filter) FROM query_stats)",
                    [],
                )?;
            }
            for s in &scope.allowed_scopes {
                tx.execute(
                    "INSERT OR IGNORE INTO query_stat_scopes VALUES(?1,?2)",
                    params![key, s],
                )?;
            }
            tx.execute("INSERT INTO query_stats SELECT ?1,COUNT(DISTINCT memory),COALESCE(SUM(len),0),COUNT(*) FROM docs WHERE scope IN(SELECT scope FROM query_stat_scopes WHERE filter=?1)",[&key])?;
        }
        tx.commit()?;
        Ok(())
    }
}
fn store_terms(c: &Connection, id: i64, terms: BTreeMap<String, i64>) -> Result<()> {
    let len: i64 = terms.values().sum();
    c.execute("UPDATE docs SET len=len+?1 WHERE id=?2", params![len, id])?;
    for (term, tf) in terms {
        let term = intern_term(c, &term)?;
        c.execute("INSERT INTO postings VALUES(?1,?2,?3) ON CONFLICT(term,doc) DO UPDATE SET tf=postings.tf+excluded.tf",params![term,id,tf])?;
    }
    Ok(())
}
fn digest_bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|p| {
            let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
            digit(p[0]) * 16 + digit(p[1])
        })
        .collect()
}
fn intern_term(c: &Connection, digest: &str) -> Result<i64> {
    let bytes = digest_bytes(digest);
    c.execute("INSERT OR IGNORE INTO lexicon(digest) VALUES(?1)", [&bytes])?;
    Ok(
        c.query_row("SELECT id FROM lexicon WHERE digest=?1", [bytes], |r| {
            r.get(0)
        })?,
    )
}
fn lookup_term(c: &Connection, digest: &str) -> Result<i64> {
    let bytes = digest_bytes(digest);
    let id: Option<i64> = c
        .query_row(
            "SELECT id FROM main.lexicon WHERE digest=?1",
            [&bytes],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = id {
        return Ok(id);
    }
    c.execute("INSERT OR IGNORE INTO term_lookup(digest,term) VALUES(?1,(SELECT COALESCE(MIN(term),0)-1 FROM term_lookup))",[&bytes])?;
    Ok(c.query_row(
        "SELECT term FROM term_lookup WHERE digest=?1",
        [bytes],
        |r| r.get(0),
    )?)
}
fn scope_key(scope: &ScopeFilter) -> Result<String> {
    let mut scopes = scope.allowed_scopes.clone();
    scopes.sort();
    scopes.dedup();
    serde_json::to_string(&scopes).map_err(|e| Error::Invalid(e.to_string()))
}
fn adjust_stats(c: &Connection, direction: i64) -> Result<()> {
    let filters = {
        let mut st = c.prepare("SELECT filter FROM query_stats")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for filter in filters {
        let(n,len,heads):(i64,i64,i64)=c.query_row("SELECT COUNT(DISTINCT memory),COALESCE(SUM(len),0),COUNT(*) FROM docs WHERE memory IN(SELECT memory FROM changed) AND scope IN(SELECT scope FROM query_stat_scopes WHERE filter=?1)",[&filter],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        c.execute(
            "UPDATE query_stats SET n=n+?2,total_len=total_len+?3,heads=heads+?4 WHERE filter=?1",
            params![filter, n * direction, len * direction, heads * direction],
        )?;
    }
    Ok(())
}
impl ReadView {
    /// Opens an existing generation without modifying the main database.
    pub fn open(path: &Path) -> SearchResult<Self> {
        let connection =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection
            .execute_batch("PRAGMA cache_size=-2048; PRAGMA temp_store=MEMORY; BEGIN DEFERRED;")?;
        let stamp =
            connection.query_row("SELECT generation,commit_id,ack FROM metadata", [], |r| {
                Ok(ViewStamp {
                    generation: r.get(0)?,
                    commit: r.get(1)?,
                    ack: r.get(2)?,
                })
            })?;
        Ok(Self { connection, stamp })
    }
    pub fn effective<'v>(
        &'v mut self,
        scope: &ScopeFilter,
        delta: LexicalDelta<'_>,
    ) -> SearchResult<EffectiveCorpus<'v>> {
        if !delta.complete {
            return Err(Error::CoverageUnknown);
        }
        let result = self.prepare_effective(scope, delta.pending, delta.removed);
        match result {
            Ok((n, avg)) => Ok(EffectiveCorpus { view: self, n, avg }),
            Err(e) => {
                let _ = self.clear_effective();
                Err(e)
            }
        }
    }
    pub fn lexical_matched(&mut self, query: &str, scope: &ScopeFilter) -> Result<u64> {
        self.effective(
            scope,
            LexicalDelta {
                pending: &[],
                removed: &[],
                complete: true,
            },
        )?
        .matched(query)
    }
    fn matched_prepared(&mut self, query: &str) -> Result<u64> {
        self.prepare_query(query)?;
        let n:i64=self.connection.query_row("SELECT COUNT(DISTINCT p.memory) FROM effective_postings p JOIN effective d USING(memory,change_id) WHERE p.term IN(SELECT term FROM query) AND d.scope IN(SELECT scope FROM scopes)",[],|r|r.get(0))?;
        Ok(n as u64)
    }
    /// IDF-weighted query-term coverage, independently normalized from BM25 ranking.
    /// Terms absent from the corpus retain their weight in the denominator.
    pub fn lexical_strength(
        &mut self,
        query: &str,
        scope: &ScopeFilter,
        limit: usize,
    ) -> Result<Vec<Hit>> {
        self.effective(
            scope,
            LexicalDelta {
                pending: &[],
                removed: &[],
                complete: true,
            },
        )?
        .strength(query, limit)
    }
    fn strength_prepared(&mut self, query: &str, limit: usize, n: f64) -> Result<Vec<Hit>> {
        self.prepare_query(query)?;
        self.connection.execute("DELETE FROM idfs", [])?;
        let mut total = 0.0;
        for term in tokens(query).keys() {
            let term = lookup_term(&self.connection, term)?;
            let df:f64=self.connection.query_row("SELECT COUNT(DISTINCT p.memory) FROM effective_postings p JOIN effective d USING(memory,change_id) WHERE p.term=?1 AND d.scope IN(SELECT scope FROM scopes)",[term],|r|r.get(0))?;
            let weight = ((n + 1.0) / (df + 1.0)).ln().max(0.0);
            total += weight;
            self.connection
                .execute("INSERT INTO idfs VALUES(?1,?2)", params![term, weight])?;
        }
        let mut st=self.connection.prepare("WITH scores AS(SELECT d.memory,d.change_id,CASE WHEN ?1>0 THEN SUM(i.idf)/?1 ELSE 0 END score FROM effective_postings p JOIN effective d USING(memory,change_id) JOIN idfs i USING(term) WHERE d.scope IN(SELECT scope FROM scopes) GROUP BY d.memory,d.change_id),ranked AS(SELECT *,ROW_NUMBER() OVER(PARTITION BY memory ORDER BY score DESC,change_id) n FROM scores) SELECT memory,change_id,score FROM ranked WHERE n=1 ORDER BY score DESC,memory LIMIT ?2")?;
        let rows = st.query_map(params![total, limit as i64], |r| {
            Ok(Hit {
                memory_id: r.get(0)?,
                change_id: r.get(1)?,
                score: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
    /// Delta must be complete for (view.ack, source_fence]. Removed/out-of-scope revisions
    /// invalidate old hits before ranking. Temporary postings remain disk backed.
    pub fn search(
        &mut self,
        query: &str,
        scope: &ScopeFilter,
        limit: usize,
        pending: &[SearchDocument],
        removed: &[String],
        delta_complete: bool,
    ) -> Result<Vec<Hit>> {
        if !delta_complete {
            return Err(Error::CoverageUnknown);
        }
        if limit > 10000 {
            return Err(Error::Invalid("query/limit exceeds bounded profile".into()));
        }
        self.effective(
            scope,
            LexicalDelta {
                pending,
                removed,
                complete: true,
            },
        )?
        .search(query, limit)
    }
    fn clear_effective(&self) -> Result<()> {
        self.connection.execute_batch("DROP VIEW IF EXISTS temp.effective_postings; DROP VIEW IF EXISTS temp.effective; DROP TABLE IF EXISTS temp.op; DROP TABLE IF EXISTS temp.overlay; DROP TABLE IF EXISTS temp.invalid; DROP TABLE IF EXISTS temp.scopes; DROP TABLE IF EXISTS temp.query; DROP TABLE IF EXISTS temp.idfs; DROP TABLE IF EXISTS temp.term_lookup;")?;
        Ok(())
    }
    fn prepare_effective(
        &mut self,
        scope: &ScopeFilter,
        pending: &[SearchDocument],
        removed: &[String],
    ) -> Result<(f64, f64)> {
        let c = &self.connection;
        c.execute_batch("DROP VIEW IF EXISTS temp.effective; DROP VIEW IF EXISTS temp.effective_postings;
          DROP TABLE IF EXISTS temp.overlay; DROP TABLE IF EXISTS temp.op; DROP TABLE IF EXISTS temp.invalid; DROP TABLE IF EXISTS temp.scopes; DROP TABLE IF EXISTS temp.query;
          CREATE TEMP TABLE overlay(memory TEXT,change_id TEXT,scope TEXT,len INTEGER,PRIMARY KEY(memory,change_id));
          DROP TABLE IF EXISTS temp.term_lookup;CREATE TEMP TABLE term_lookup(digest BLOB PRIMARY KEY,term INTEGER UNIQUE);
          CREATE TEMP TABLE op(term INTEGER,memory TEXT,change_id TEXT,tf INTEGER,PRIMARY KEY(term,memory,change_id)) WITHOUT ROWID;
          CREATE TEMP TABLE invalid(memory TEXT PRIMARY KEY); CREATE TEMP TABLE scopes(scope TEXT PRIMARY KEY);
          CREATE TEMP TABLE query(term INTEGER PRIMARY KEY); DROP TABLE IF EXISTS temp.idfs; CREATE TEMP TABLE idfs(term INTEGER PRIMARY KEY,idf REAL);
          CREATE TEMP VIEW effective AS SELECT memory,change_id,scope,len FROM main.docs WHERE memory NOT IN(SELECT memory FROM invalid) UNION ALL SELECT * FROM overlay;
          CREATE TEMP VIEW effective_postings AS SELECT p.term,d.memory,d.change_id,p.tf FROM main.postings p JOIN main.docs d ON d.id=p.doc WHERE d.memory NOT IN(SELECT memory FROM invalid) UNION ALL SELECT * FROM op;")?;
        if pending.is_empty() && removed.is_empty() {
            c.execute_batch("DROP VIEW effective;DROP VIEW effective_postings;CREATE TEMP VIEW effective AS SELECT memory,change_id,scope,len FROM main.docs;CREATE TEMP VIEW effective_postings AS SELECT p.term,d.memory,d.change_id,p.tf FROM query q JOIN main.postings p ON p.term=q.term JOIN main.docs d ON d.id=p.doc;")?;
        }
        for s in &scope.allowed_scopes {
            c.execute("INSERT OR IGNORE INTO scopes VALUES(?1)", [s])?;
        }
        for id in removed
            .iter()
            .map(String::as_str)
            .chain(pending.iter().map(|d| d.memory_id.as_str()))
        {
            c.execute("INSERT OR IGNORE INTO invalid VALUES(?1)", [id])?;
        }
        for d in pending {
            let terms = tokens(&d.body);
            let len: i64 = terms.values().sum();
            c.execute(
                "INSERT INTO overlay VALUES(?1,?2,?3,?4)",
                params![d.memory_id, d.change_id, d.scope, len],
            )?;
            for (term, tf) in terms {
                let term = lookup_term(c, &term)?;
                c.execute(
                    "INSERT INTO op VALUES(?1,?2,?3,?4)",
                    params![term, d.memory_id, d.change_id, tf],
                )?;
            }
        }
        let cached: Option<(f64, f64)> = if pending.is_empty() && removed.is_empty() {
            c.query_row("SELECT n,CASE WHEN heads>0 THEN CAST(total_len AS REAL)/heads ELSE 1 END FROM query_stats WHERE filter=?1",[scope_key(scope)?],|r|Ok((r.get(0)?,r.get(1)?))).optional()?
        } else {
            None
        };
        let (n,avg):(f64,f64)=match cached{Some(v)=>v,None=>c.query_row("SELECT COUNT(DISTINCT memory),COALESCE(AVG(len),1) FROM effective WHERE scope IN(SELECT scope FROM scopes)",[],|r|Ok((r.get(0)?,r.get(1)?)))?};
        Ok((n, avg))
    }
    fn prepare_query(&self, query: &str) -> Result<()> {
        let c = &self.connection;
        c.execute("DELETE FROM query", [])?;
        for term in tokens(query).keys() {
            let term = lookup_term(c, term)?;
            c.execute("INSERT INTO query VALUES(?1)", [term])?;
        }
        Ok(())
    }
    fn search_prepared(&mut self, query: &str, limit: usize, n: f64, avg: f64) -> Result<Vec<Hit>> {
        if limit > 10000 {
            return Err(Error::Invalid("query/limit exceeds bounded profile".into()));
        }
        self.prepare_query(query)?;
        let c = &self.connection;
        c.execute("DELETE FROM idfs", [])?;
        {
            let mut st=c.prepare("SELECT p.term,COUNT(DISTINCT p.memory) FROM effective_postings p JOIN effective d USING(memory,change_id) WHERE p.term IN(SELECT term FROM query) AND d.scope IN(SELECT scope FROM scopes) GROUP BY p.term")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let term: i64 = r.get(0)?;
                let df: f64 = r.get(1)?;
                let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                c.execute("INSERT INTO idfs VALUES(?1,?2)", params![term, idf])?;
            }
        }
        let mut st=c.prepare("WITH scores AS (SELECT d.memory,d.change_id,SUM(i.idf*p.tf*2.2/(p.tf+1.2*(0.25+0.75*d.len/?1))) score FROM effective_postings p JOIN effective d USING(memory,change_id) JOIN idfs i USING(term) WHERE d.scope IN(SELECT scope FROM scopes) GROUP BY d.memory,d.change_id), ranked AS (SELECT memory,change_id,score,ROW_NUMBER() OVER(PARTITION BY memory ORDER BY score DESC,change_id ASC) n FROM scores) SELECT memory,change_id,score FROM ranked WHERE n=1 ORDER BY score DESC,memory ASC LIMIT ?2")?;
        let rows = st.query_map(params![avg.max(1.0), limit as i64], |r| {
            Ok(Hit {
                memory_id: r.get(0)?,
                change_id: r.get(1)?,
                score: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

impl EffectiveCorpus<'_> {
    pub fn search(&mut self, query: &str, limit: usize) -> SearchResult<Vec<Hit>> {
        self.view.search_prepared(query, limit, self.n, self.avg)
    }
    pub fn strength(&mut self, query: &str, limit: usize) -> SearchResult<Vec<Hit>> {
        self.view.strength_prepared(query, limit, self.n)
    }
    pub fn matched(&mut self, query: &str) -> SearchResult<u64> {
        self.view.matched_prepared(query)
    }
}
impl Drop for EffectiveCorpus<'_> {
    fn drop(&mut self) {
        let _ = self.view.clear_effective();
    }
}

/// Each channel is deduplicated by memory before assigning ranks. Ties use stable ID.
pub fn reciprocal_rank_fusion(channels: &[Vec<Hit>]) -> Vec<Hit> {
    let mut combined: BTreeMap<String, Hit> = BTreeMap::new();
    for channel in channels {
        let mut best: BTreeMap<String, Hit> = BTreeMap::new();
        for h in channel {
            if !h.score.is_finite() {
                continue;
            }
            let e = best.entry(h.memory_id.clone()).or_insert(h.clone());
            if h.score > e.score {
                *e = h.clone();
            }
        }
        let mut ranked: Vec<_> = best.into_values().collect();
        ranked.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.memory_id.cmp(&b.memory_id))
        });
        for (rank, h) in ranked.into_iter().enumerate() {
            let e = combined
                .entry(h.memory_id.clone())
                .or_insert(Hit { score: 0.0, ..h });
            e.score += 1.0 / (61.0 + rank as f64);
        }
    }
    let mut result: Vec<_> = combined.into_values().collect();
    result.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.memory_id.cmp(&b.memory_id))
    });
    result
}
pub fn cosine(a: &[f32], b: &[f32]) -> Result<f64> {
    if a.is_empty() || a.len() != b.len() || a.iter().chain(b).any(|v| !v.is_finite()) {
        return Err(Error::Invalid("invalid vector dimensions/value".into()));
    }
    let (dot, aa, bb) = a
        .iter()
        .zip(b)
        .fold((0.0, 0.0, 0.0), |(d, x, y), (&a, &b)| {
            (
                d + a as f64 * b as f64,
                x + (a as f64).powi(2),
                y + (b as f64).powi(2),
            )
        });
    if aa == 0.0 || bb == 0.0 {
        return Err(Error::Invalid("zero vector".into()));
    }
    Ok(dot / (aa * bb).sqrt())
}
pub trait VectorBackend {
    fn search(&self, query: &[f32], scope: &ScopeFilter, limit: usize) -> Result<Vec<Hit>>;
}

#[derive(Clone, Debug)]
pub struct VectorRecord {
    pub memory_id: String,
    pub change_id: String,
    pub section: String,
    pub scope: String,
    pub vector: Vec<f32>,
}
/// Exact disk scan baseline, bounded RAM. This is not an ANN scalability claim.
pub struct ExactVectorIndex {
    connection: Connection,
    path: std::path::PathBuf,
    dimension: usize,
}
pub struct VectorReadView {
    connection: Connection,
    pub stamp: ViewStamp,
    pub model_digest: String,
    pub config_digest: String,
    dimension: usize,
}
impl ExactVectorIndex {
    pub fn commit_stream(
        &mut self,
        source_generation: i64,
        changed_ids: impl Iterator<Item = Result<String>>,
        records: impl Iterator<Item = Result<VectorRecord>>,
    ) -> Result<()> {
        let tx = self.connection.transaction()?;
        let ack: i64 = tx.query_row("SELECT ack FROM vector_meta", [], |r| r.get(0))?;
        if source_generation < ack {
            return Err(Error::Invalid("watermark regression".into()));
        }
        for id in changed_ids {
            let id = id?;
            tx.execute("DELETE FROM vector_buckets WHERE memory=?1", [&id])?;
            tx.execute("DELETE FROM vectors WHERE memory=?1", [&id])?;
        }
        for record in records {
            let r = record?;
            if r.vector.len() != self.dimension {
                return Err(Error::Invalid("vector dimension mismatch".into()));
            }
            cosine(&r.vector, &r.vector)?;
            let bytes: Vec<u8> = r.vector.iter().flat_map(|v| v.to_le_bytes()).collect();
            tx.execute(
                "INSERT INTO vectors VALUES(?1,?2,?3,?4,?5)",
                params![r.memory_id, r.change_id, r.section, r.scope, bytes],
            )?;
            ann::insert(&tx, &r)?;
        }
        if tx.execute("UPDATE vector_meta SET ack=?1,commit_id=commit_id+1 WHERE commit_id<9223372036854775807",[source_generation])?!=1{return Err(Error::Invalid("index counter overflow".into()));}
        tx.commit()?;
        Ok(())
    }
    pub fn open(
        path: impl AsRef<Path>,
        model: &str,
        config: &str,
        dimension: usize,
    ) -> Result<Self> {
        if dimension == 0 || dimension > 65536 {
            return Err(Error::Invalid("dimension out of range".into()));
        }
        let c = Connection::open(path.as_ref())?;
        configure(&c)?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
  CREATE TABLE IF NOT EXISTS vector_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),generation INTEGER NOT NULL,commit_id INTEGER NOT NULL,ack INTEGER NOT NULL,model TEXT NOT NULL,config TEXT NOT NULL,dimension INTEGER NOT NULL);
  CREATE TABLE IF NOT EXISTS vectors(memory TEXT NOT NULL,change_id TEXT NOT NULL,section TEXT NOT NULL,scope TEXT NOT NULL,vector BLOB NOT NULL,PRIMARY KEY(memory,change_id,section)) WITHOUT ROWID;")?;
        ann::initialize(&c)?;
        c.execute(
            "INSERT OR IGNORE INTO vector_meta VALUES(1,1,0,0,?1,?2,?3)",
            params![model, config, dimension as i64],
        )?;
        let (m, cfg, dim): (String, String, i64) =
            c.query_row("SELECT model,config,dimension FROM vector_meta", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        if m != model || cfg != config || dim != dimension as i64 {
            return Err(Error::Invalid(
                "model/config/dimension generation mismatch; rebuild separately".into(),
            ));
        }
        Ok(Self {
            connection: c,
            path: path.as_ref().to_path_buf(),
            dimension,
        })
    }
    pub fn commit(
        &mut self,
        source_generation: i64,
        records: &[VectorRecord],
        removed: &[String],
    ) -> Result<()> {
        for r in records {
            if r.vector.len() != self.dimension {
                return Err(Error::Invalid("dimension mismatch".into()));
            }
            cosine(&r.vector, &r.vector)?;
        }
        let tx = self.connection.transaction()?;
        let ack: i64 = tx.query_row("SELECT ack FROM vector_meta", [], |r| r.get(0))?;
        if source_generation < ack {
            return Err(Error::Invalid("watermark regression".into()));
        }
        for id in removed
            .iter()
            .map(String::as_str)
            .chain(records.iter().map(|r| r.memory_id.as_str()))
        {
            tx.execute("DELETE FROM vector_buckets WHERE memory=?1", [id])?;
            tx.execute("DELETE FROM vectors WHERE memory=?1", [id])?;
        }
        for r in records {
            let bytes: Vec<u8> = r.vector.iter().flat_map(|v| v.to_le_bytes()).collect();
            tx.execute(
                "INSERT INTO vectors VALUES(?1,?2,?3,?4,?5)",
                params![r.memory_id, r.change_id, r.section, r.scope, bytes],
            )?;
            ann::insert(&tx, r)?;
        }
        if tx.execute(
            "UPDATE vector_meta SET commit_id=commit_id+1,ack=?1 WHERE commit_id<9223372036854775807",
            [source_generation],
        )?!=1 {return Err(Error::Invalid("index commit counter overflow".into()));}
        tx.commit()?;
        Ok(())
    }
    pub fn read_view(&self) -> Result<VectorReadView> {
        let c = Connection::open(&self.path)?;
        configure(&c)?;
        c.execute_batch("BEGIN DEFERRED")?;
        let (stamp, model_digest, config_digest) = c.query_row(
            "SELECT generation,commit_id,ack,model,config FROM vector_meta",
            [],
            |r| {
                Ok((
                    ViewStamp {
                        generation: r.get(0)?,
                        commit: r.get(1)?,
                        ack: r.get(2)?,
                    },
                    r.get(3)?,
                    r.get(4)?,
                ))
            },
        )?;
        Ok(VectorReadView {
            connection: c,
            stamp,
            model_digest,
            config_digest,
            dimension: self.dimension,
        })
    }
}
fn keep_best(hits: &mut Vec<Hit>, h: Hit, limit: usize) {
    if limit == 0 {
        return;
    }
    if let Some(old) = hits.iter_mut().find(|v| v.memory_id == h.memory_id) {
        if h.score > old.score {
            *old = h;
        }
    } else {
        hits.push(h);
    }
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.memory_id.cmp(&b.memory_id))
    });
    hits.truncate(limit);
}
impl VectorReadView {
    /// Every pending vector is evaluated regardless of lexical match. The caller obtains
    /// all pending embeddings (including queued READY work) before calling this method.
    pub fn search_with_overlay(
        &self,
        query: &[f32],
        scope: &ScopeFilter,
        limit: usize,
        pending: &[VectorRecord],
        removed: &[String],
        delta_complete: bool,
    ) -> Result<Vec<Hit>> {
        if !delta_complete {
            return Err(Error::CoverageUnknown);
        }
        if query.len() != self.dimension || limit > 10000 {
            return Err(Error::Invalid("dimension/limit mismatch".into()));
        }
        cosine(query, query)?;
        self.connection.execute_batch("DROP TABLE IF EXISTS temp.v_invalid;DROP TABLE IF EXISTS temp.v_scopes;CREATE TEMP TABLE v_invalid(memory TEXT PRIMARY KEY);CREATE TEMP TABLE v_scopes(scope TEXT PRIMARY KEY);")?;
        for id in removed
            .iter()
            .map(String::as_str)
            .chain(pending.iter().map(|p| p.memory_id.as_str()))
        {
            self.connection
                .execute("INSERT OR IGNORE INTO v_invalid VALUES(?1)", [id])?;
        }
        for s in &scope.allowed_scopes {
            self.connection
                .execute("INSERT OR IGNORE INTO v_scopes VALUES(?1)", [s])?;
        }
        let mut st=self.connection.prepare("SELECT memory,change_id,vector FROM vectors WHERE scope IN(SELECT scope FROM v_scopes) AND memory NOT IN(SELECT memory FROM v_invalid) ORDER BY memory,change_id,section")?;
        let mut rows = st.query([])?;
        let mut hits = Vec::with_capacity(limit.saturating_add(1));
        while let Some(row) = rows.next()? {
            let bytes: Vec<u8> = row.get(2)?;
            if bytes.len() != self.dimension * 4 {
                return Err(Error::Invalid("corrupt vector length".into()));
            }
            let vector: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            keep_best(
                &mut hits,
                Hit {
                    memory_id: row.get(0)?,
                    change_id: row.get(1)?,
                    score: cosine(query, &vector)?,
                },
                limit,
            );
        }
        for p in pending {
            if scope.allowed_scopes.contains(&p.scope) {
                keep_best(
                    &mut hits,
                    Hit {
                        memory_id: p.memory_id.clone(),
                        change_id: p.change_id.clone(),
                        score: cosine(query, &p.vector)?,
                    },
                    limit,
                );
            }
        }
        Ok(hits)
    }
}
impl VectorBackend for VectorReadView {
    fn search(&self, query: &[f32], scope: &ScopeFilter, limit: usize) -> Result<Vec<Hit>> {
        self.search_with_overlay(query, scope, limit, &[], &[], true)
    }
}

#[cfg(test)]
mod tests;
