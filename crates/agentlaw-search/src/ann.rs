//! Disk-resident multi-probe random-hyperplane LSH. Candidate retrieval is approximate;
//! the final section score and per-memory aggregation use exact float32 vectors.
use super::*;
const TABLES: u64 = 12;
const BITS: u32 = 12;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnnMetrics {
    pub memory_candidates: usize,
    pub candidates_scored: usize,
    pub probes: usize,
    pub candidate_limit: usize,
}
pub(super) fn initialize(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS vector_buckets(table_id INTEGER NOT NULL,bucket INTEGER NOT NULL,scope TEXT NOT NULL,memory TEXT NOT NULL,change_id TEXT NOT NULL,section TEXT NOT NULL,PRIMARY KEY(table_id,bucket,scope,memory,change_id,section)) WITHOUT ROWID;CREATE INDEX IF NOT EXISTS bucket_memory ON vector_buckets(memory);CREATE TABLE IF NOT EXISTS ann_profile(singleton INTEGER PRIMARY KEY,version INTEGER);INSERT OR IGNORE INTO ann_profile SELECT 1,CASE WHEN EXISTS(SELECT 1 FROM vectors) THEN -1 ELSE 1 END;")?;
    Ok(())
}
fn mix(mut n: u64) -> u64 {
    n = n.wrapping_add(0x9e3779b97f4a7c15);
    n = (n ^ (n >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    n = (n ^ (n >> 27)).wrapping_mul(0x94d049bb133111eb);
    n ^ (n >> 31)
}
fn signatures(v: &[f32]) -> Vec<i64> {
    (0..TABLES)
        .map(|table| {
            let mut bucket = 0;
            for bit in 0..BITS {
                let mut dot = 0.0f64;
                for (dim, value) in v.iter().enumerate() {
                    let sign =
                        if mix((table * BITS as u64 + bit as u64) * 65537 + dim as u64) & 1 == 0 {
                            1.0
                        } else {
                            -1.0
                        };
                    dot += *value as f64 * sign;
                }
                if dot >= 0.0 {
                    bucket |= 1 << bit;
                }
            }
            bucket
        })
        .collect()
}
pub(super) fn insert(c: &Connection, r: &VectorRecord) -> Result<()> {
    for (table, bucket) in signatures(&r.vector).into_iter().enumerate() {
        c.execute(
            "INSERT INTO vector_buckets VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                table as i64,
                bucket,
                r.scope,
                r.memory_id,
                r.change_id,
                r.section
            ],
        )?;
    }
    Ok(())
}
impl VectorReadView {
    /// Fixed retrieval profile (12 tables, Hamming radius one, 4096 minimum candidates).
    /// No wall-clock deadline silently truncates a requested channel. Scope is filtered
    /// inside bucket probes, before the candidate cap and exact re-scoring.
    pub fn search_ann(
        &self,
        query: &[f32],
        scope: &ScopeFilter,
        limit: usize,
    ) -> Result<(Vec<Hit>, AnnMetrics)> {
        if query.len() != self.dimension || limit > 10000 {
            return Err(Error::Invalid("ANN dimension/limit mismatch".into()));
        }
        cosine(query, query)?;
        let candidate_limit = 4096usize.max(limit.saturating_mul(8));
        let c = &self.connection;
        let profile: i64 = c.query_row("SELECT version FROM ann_profile", [], |r| r.get(0))?;
        if profile != 1 {
            return Err(Error::CoverageUnknown);
        }
        c.execute_batch("DROP TABLE IF EXISTS temp.ann_scopes;DROP TABLE IF EXISTS temp.ann_probes;DROP TABLE IF EXISTS temp.ann_candidates;CREATE TEMP TABLE ann_scopes(scope TEXT PRIMARY KEY);CREATE TEMP TABLE ann_probes(table_id INTEGER,bucket INTEGER,PRIMARY KEY(table_id,bucket));CREATE TEMP TABLE ann_candidates(memory TEXT,change_id TEXT,section TEXT,votes INTEGER,PRIMARY KEY(memory,change_id,section));")?;
        for s in &scope.allowed_scopes {
            c.execute("INSERT OR IGNORE INTO ann_scopes VALUES(?1)", [s])?;
        }
        for (table, bucket) in signatures(query).into_iter().enumerate() {
            c.execute(
                "INSERT INTO ann_probes VALUES(?1,?2)",
                params![table as i64, bucket],
            )?;
            for bit in 0..BITS {
                c.execute(
                    "INSERT INTO ann_probes VALUES(?1,?2)",
                    params![table as i64, bucket ^ (1 << bit)],
                )?;
            }
        }
        c.execute("INSERT INTO ann_candidates SELECT b.memory,b.change_id,b.section,COUNT(*) votes FROM ann_probes p CROSS JOIN ann_scopes s JOIN vector_buckets b ON b.table_id=p.table_id AND b.bucket=p.bucket AND b.scope=s.scope GROUP BY b.memory,b.change_id,b.section ORDER BY votes DESC,b.memory,b.change_id,b.section LIMIT ?1",[candidate_limit as i64])?;
        let mut st=c.prepare("SELECT v.memory,v.change_id,v.vector FROM ann_candidates a JOIN vectors v USING(memory,change_id,section) ORDER BY a.votes DESC,v.memory,v.change_id,v.section")?;
        let mut rows = st.query([])?;
        let mut hits = Vec::new();
        let mut candidates_scored = 0;
        while let Some(r) = rows.next()? {
            let bytes: Vec<u8> = r.get(2)?;
            if bytes.len() != self.dimension * 4 {
                return Err(Error::Invalid("corrupt ANN vector".into()));
            }
            let vector: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            candidates_scored += 1;
            keep_best(
                &mut hits,
                Hit {
                    memory_id: r.get(0)?,
                    change_id: r.get(1)?,
                    score: cosine(query, &vector)?,
                },
                limit,
            );
        }
        Ok((
            hits,
            AnnMetrics {
                memory_candidates: c.query_row(
                    "SELECT COUNT(DISTINCT memory) FROM ann_candidates",
                    [],
                    |r| r.get::<_, i64>(0),
                )? as usize,
                candidates_scored,
                probes: TABLES as usize * (BITS as usize + 1),
                candidate_limit,
            },
        ))
    }
}
