//! Local ledger and connection policy. File publication remains separate.
use crate::{Error, Result, Store};
use std::path::Path;
pub(super) fn open(path: &Path) -> Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS publications(operation_id TEXT PRIMARY KEY, receipt TEXT NOT NULL, manifest TEXT NOT NULL, sequence BLOB NOT NULL CHECK(length(sequence)=8));")?;
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS publications_sequence ON publications(sequence);",
    )?;
    let missing_sequence: i64 = conn.query_row(
        "SELECT COUNT(*) FROM publications WHERE sequence IS NULL",
        [],
        |row| row.get(0),
    )?;
    if missing_sequence != 0 {
        return Err(Error::Corrupt(
            "publication ledger has rows without sequence".into(),
        ));
    }
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
