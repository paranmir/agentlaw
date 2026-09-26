//! Local ledger migration and connection policy. File publication remains separate.
use crate::{Error, PublishReceipt, Result, Store};
use std::path::Path;
pub(super) fn open(path: &Path) -> Result<rusqlite::Connection> {
    let mut conn = rusqlite::Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS publications(operation_id TEXT PRIMARY KEY, receipt TEXT NOT NULL, manifest TEXT NOT NULL, sequence BLOB CHECK(sequence IS NULL OR length(sequence)=8));")?;
    let has_sequence = {
        let mut q = conn.prepare("PRAGMA table_info(publications)")?;
        let names = q.query_map([], |r| r.get::<_, String>(1))?;
        names
            .collect::<std::result::Result<Vec<_>, _>>()?
            .iter()
            .any(|n| n == "sequence")
    };
    if !has_sequence {
        conn.execute_batch("ALTER TABLE publications ADD COLUMN sequence BLOB CHECK(sequence IS NULL OR length(sequence)=8);")?;
    }
    let tx = conn.transaction()?;
    // Legacy rewrite rows are migrated one at a time, never an all-history RAM collection.
    {
        let mut query =
            tx.prepare("SELECT operation_id,receipt FROM publications WHERE sequence IS NULL")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let operation: String = row.get(0)?;
            let raw: String = row.get(1)?;
            let receipt: PublishReceipt = serde_json::from_str(&raw)?;
            tx.execute(
                "UPDATE publications SET sequence=?1 WHERE operation_id=?2",
                rusqlite::params![receipt.generation.to_be_bytes().as_slice(), operation],
            )?;
        }
    }
    tx.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS publications_sequence ON publications(sequence);",
    )?;
    tx.commit()?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS canonical_change_ids(change_id TEXT PRIMARY KEY); CREATE TABLE IF NOT EXISTS registry_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1), complete INTEGER NOT NULL CHECK(complete IN(0,1)));")?;
    Ok(conn)
}
pub(super) fn reject_existing_changes(
    store: &Store,
    ids: &std::collections::BTreeSet<String>,
) -> Result<()> {
    let mut conn = open(&store.local.join("journal.sqlite"))?;
    let complete = conn
        .query_row(
            "SELECT complete FROM registry_state WHERE singleton=1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        == 1;
    if !complete {
        let tx = conn.transaction()?;
        store.scan_history(|frame| {
            if frame.kind == "change_descriptor" {
                tx.execute(
                    "INSERT OR IGNORE INTO canonical_change_ids(change_id) VALUES (?1)",
                    [frame.key],
                )?;
            }
            Ok(())
        })?;
        tx.execute(
            "INSERT OR REPLACE INTO registry_state(singleton,complete) VALUES(1,1)",
            [],
        )?;
        tx.commit()?;
    }
    let mut query =
        conn.prepare("SELECT EXISTS(SELECT 1 FROM canonical_change_ids WHERE change_id=?1)")?;
    for id in ids {
        if query.query_row([id], |r| r.get::<_, bool>(0))? {
            return Err(Error::Corrupt(
                "immutable historical change identity reused".into(),
            ));
        }
    }
    Ok(())
}
