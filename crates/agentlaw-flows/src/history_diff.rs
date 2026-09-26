//! Lossless one-hunk diff with a disk line index, bounded even for a giant line.
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Writes JSON string contents (without quotes), preserving UTF-8 byte sequences.
pub struct JsonEscape<'a, W: Write>(pub &'a mut W);
impl<W: Write> Write for JsonEscape<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut start = 0;
        for (i, b) in bytes.iter().copied().enumerate() {
            let escaped = match b {
                b'"' => Some(b"\\\"".as_slice()),
                b'\\' => Some(b"\\\\".as_slice()),
                b'\n' => Some(b"\\n".as_slice()),
                b'\r' => Some(b"\\r".as_slice()),
                b'\t' => Some(b"\\t".as_slice()),
                _ => None,
            };
            if let Some(s) = escaped {
                self.0.write_all(&bytes[start..i])?;
                self.0.write_all(s)?;
                start = i + 1;
            } else if b < 0x20 {
                self.0.write_all(&bytes[start..i])?;
                write!(self.0, "\\u{b:04x}")?;
                start = i + 1;
            }
        }
        self.0.write_all(&bytes[start..])?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
pub fn json_string(input: &mut impl Read, out: &mut impl Write) -> io::Result<()> {
    out.write_all(b"\"")?;
    io::copy(input, &mut JsonEscape(out))?;
    out.write_all(b"\"")
}
fn index(db: &Connection, side: i64, path: &Path) -> Result<u64> {
    let mut file = File::open(path)?;
    let mut buffer = [0u8; 65536];
    let mut offset = 0u64;
    let mut start = 0u64;
    let mut number = 0u64;
    let mut digest = Sha256::new();
    let mut insert = db.prepare_cached("INSERT INTO lines VALUES(?1,?2,?3,?4,?5)")?;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let mut consumed = 0;
        for (i, b) in buffer[..n].iter().copied().enumerate() {
            if b == b'\n' {
                digest.update(&buffer[consumed..=i]);
                let end = offset
                    .checked_add(u64::try_from(i + 1)?)
                    .ok_or("offset overflow")?;
                insert.execute(params![
                    side,
                    number,
                    start,
                    end,
                    format!("{:x}", digest.finalize_reset())
                ])?;
                number = number.checked_add(1).ok_or("line count overflow")?;
                start = end;
                consumed = i + 1;
            }
        }
        digest.update(&buffer[consumed..n]);
        offset = offset
            .checked_add(u64::try_from(n)?)
            .ok_or("offset overflow")?;
    }
    if start < offset {
        insert.execute(params![
            side,
            number,
            start,
            offset,
            format!("{:x}", digest.finalize())
        ])?;
        number = number.checked_add(1).ok_or("line count overflow")?;
    }
    Ok(number)
}
fn emit(
    db: &Connection,
    side: i64,
    path: &Path,
    start: u64,
    end: u64,
    prefix: u8,
    out: &mut impl Write,
) -> Result<()> {
    if start == end {
        return Ok(());
    }
    let (offset,limit):(u64,u64)=db.query_row("SELECT a.start,b.end FROM lines a JOIN lines b ON b.side=a.side WHERE a.side=?1 AND a.ordinal=?2 AND b.ordinal=?3",params![side,start,end-1],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let mut input = File::open(path)?;
    input.seek(SeekFrom::Start(offset))?;
    let mut input = input.take(limit - offset);
    let mut buf = [0u8; 65536];
    let mut beginning = true;
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut consumed = 0;
        for (i, b) in buf[..n].iter().copied().enumerate() {
            if beginning {
                out.write_all(&[prefix])?;
                beginning = false;
            }
            if b == b'\n' {
                out.write_all(&buf[consumed..=i])?;
                consumed = i + 1;
                beginning = true;
            }
        }
        out.write_all(&buf[consumed..n])?;
    }
    if !beginning {
        out.write_all(b"\n\\ No newline at end of file\n")?;
    }
    Ok(())
}
pub fn write_diff(before: &Path, after: &Path, scratch: &Path, out: &mut impl Write) -> Result<()> {
    let db = Connection::open(scratch)?;
    db.execute_batch("PRAGMA temp_store=FILE; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; CREATE TABLE lines(side INTEGER,ordinal INTEGER,start INTEGER,end INTEGER,digest TEXT,PRIMARY KEY(side,ordinal)); BEGIN")?;
    let a = index(&db, 0, before)?;
    let b = index(&db, 1, after)?;
    db.execute_batch("COMMIT")?;
    let prefix:u64=db.query_row("SELECT coalesce(min(a.ordinal),?1) FROM lines a JOIN lines b ON b.side=1 AND b.ordinal=a.ordinal WHERE a.side=0 AND (a.digest<>b.digest OR a.end-a.start<>b.end-b.start)",[a.min(b)],|r|r.get(0))?;
    if prefix == a && prefix == b {
        return Ok(());
    }
    let suffix:u64=db.query_row("SELECT coalesce(min(?1-1-a.ordinal),?3) FROM lines a JOIN lines b ON b.side=1 AND ?1-1-a.ordinal=?2-1-b.ordinal WHERE a.side=0 AND a.ordinal>=?4 AND b.ordinal>=?4 AND (a.digest<>b.digest OR a.end-a.start<>b.end-b.start)",params![a,b,a.min(b)-prefix,prefix],|r|r.get(0))?;
    let start = prefix.saturating_sub(3);
    let trailing = suffix.min(3);
    let ac = a - suffix;
    let bc = b - suffix;
    let aend = ac + trailing;
    let bend = bc + trailing;
    let alen = aend - start;
    let blen = bend - start;
    writeln!(
        out,
        "--- before\n+++ after\n@@ -{},{} +{},{} @@",
        if alen == 0 { start } else { start + 1 },
        alen,
        if blen == 0 { start } else { start + 1 },
        blen
    )?;
    emit(&db, 0, before, start, prefix, b' ', out)?;
    emit(&db, 0, before, prefix, ac, b'-', out)?;
    emit(&db, 1, after, prefix, bc, b'+', out)?;
    emit(&db, 0, before, ac, aend, b' ', out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disk_diff_matches_in_memory_reference_including_large_lines() {
        let long = "가".repeat(50000);
        for (i, (a, b)) in [
            ("".into(), "x".into()),
            ("x".into(), "".into()),
            ("keep\r\nold\r\nend".into(), "keep\r\nnew\r\nend".into()),
            (format!("{long}\nold\n"), format!("{long}\nnew\n")),
            ("same\n".into(), "same\n".into()),
            ("a\nb\nc\nd\nend\n".into(), "a\nb\nX\nc\nd\nend\n".into()),
        ]
        .into_iter()
        .enumerate()
        {
            let temp = tempfile::tempdir().unwrap();
            let before = temp.path().join("a");
            let after = temp.path().join("b");
            std::fs::write(&before, &a).unwrap();
            std::fs::write(&after, &b).unwrap();
            let mut result = Vec::new();
            write_diff(
                &before,
                &after,
                &temp.path().join(format!("{i}.sqlite")),
                &mut result,
            )
            .unwrap();
            assert_eq!(
                String::from_utf8(result).unwrap(),
                crate::history::full_diff(&a, &b)
            );
        }
    }
    #[test]
    fn string_escape_is_valid_and_preserves_utf8_control_bytes() {
        let original = "한글\n\r\t\0\"\\";
        let mut bytes = Vec::new();
        json_string(&mut original.as_bytes(), &mut bytes).unwrap();
        assert_eq!(serde_json::from_slice::<String>(&bytes).unwrap(), original);
    }
}
