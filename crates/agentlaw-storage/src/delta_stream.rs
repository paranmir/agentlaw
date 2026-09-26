//! Strict streaming utf8-splice-v1 validation, including arbitrarily long insert strings.
use super::*;
use std::io::BufRead;
struct Json<R> {
    r: R,
}
impl<R: BufRead> Json<R> {
    fn byte(&mut self) -> Result<u8> {
        let mut b = [0];
        self.r.read_exact(&mut b)?;
        Ok(b[0])
    }
    fn ws(&mut self) -> Result<()> {
        loop {
            let b = self.r.fill_buf()?;
            if b.first().is_some_and(|c| c.is_ascii_whitespace()) {
                self.r.consume(1)
            } else {
                return Ok(());
            }
        }
    }
    fn peek(&mut self) -> Result<Option<u8>> {
        self.ws()?;
        Ok(self.r.fill_buf()?.first().copied())
    }
    fn expect(&mut self, b: u8) -> Result<()> {
        self.ws()?;
        if self.byte()? != b {
            return Err(Error::Corrupt("delta JSON token".into()));
        }
        Ok(())
    }
    fn hex(&mut self) -> Result<u16> {
        let mut v = 0u16;
        for _ in 0..4 {
            let c = self.byte()?;
            let n = (c as char)
                .to_digit(16)
                .ok_or_else(|| Error::Corrupt("JSON Unicode escape".into()))?;
            v = (v << 4) | n as u16;
        }
        Ok(v)
    }
    fn string(&mut self, out: &mut impl Write) -> Result<()> {
        self.expect(b'"')?;
        loop {
            let b = self.byte()?;
            match b {
                b'"' => return Ok(()),
                b'\\' => {
                    let e = self.byte()?;
                    match e {
                        b'"' | b'\\' | b'/' => out.write_all(&[e])?,
                        b'b' => out.write_all(&[8])?,
                        b'f' => out.write_all(&[12])?,
                        b'n' => out.write_all(b"\n")?,
                        b'r' => out.write_all(b"\r")?,
                        b't' => out.write_all(b"\t")?,
                        b'u' => {
                            let first = self.hex()?;
                            let cp = if (0xd800..=0xdbff).contains(&first) {
                                if self.byte()? != b'\\' || self.byte()? != b'u' {
                                    return Err(Error::Corrupt("unpaired high surrogate".into()));
                                }
                                let second = self.hex()?;
                                if !(0xdc00..=0xdfff).contains(&second) {
                                    return Err(Error::Corrupt("unpaired surrogate".into()));
                                }
                                0x10000
                                    + ((u32::from(first) - 0xd800) << 10)
                                    + (u32::from(second) - 0xdc00)
                            } else {
                                u32::from(first)
                            };
                            let c = char::from_u32(cp)
                                .ok_or_else(|| Error::Corrupt("invalid Unicode escape".into()))?;
                            let mut buf = [0; 4];
                            out.write_all(c.encode_utf8(&mut buf).as_bytes())?;
                        }
                        _ => return Err(Error::Corrupt("JSON escape".into())),
                    }
                }
                0..=31 => return Err(Error::Corrupt("unescaped JSON control".into())),
                _ => out.write_all(&[b])?,
            }
        }
    }
    fn small(&mut self) -> Result<String> {
        struct Small(Vec<u8>);
        impl Write for Small {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                if self.0.len() + b.len() > 128 {
                    return Err(std::io::Error::other("JSON small field overflow"));
                }
                self.0.extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut s = Small(Vec::new());
        self.string(&mut s)?;
        String::from_utf8(s.0).map_err(|_| Error::Corrupt("JSON string UTF8".into()))
    }
    fn number_string(&mut self) -> Result<u64> {
        let s = self.small()?;
        if s.is_empty()
            || s.len() > 1 && s.starts_with('0')
            || !s.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(Error::Corrupt("delta offset spelling".into()));
        }
        s.parse()
            .map_err(|_| Error::Corrupt("delta offset overflow".into()))
    }
}
struct Compare<R> {
    expected: R,
}
impl<R: Read> Write for Compare<R> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut buf = [0u8; 65536];
        for part in bytes.chunks(buf.len()) {
            self.expected.read_exact(&mut buf[..part.len()])?;
            if part != &buf[..part.len()] {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "delta/checkpoint mismatch",
                ));
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn boundary(r: &mut impl BufRead) -> Result<()> {
    if r.fill_buf()?.first().is_some_and(|b| b & 0xc0 == 0x80) {
        return Err(Error::Corrupt("splice splits UTF8 codepoint".into()));
    }
    Ok(())
}
pub(super) fn verify(
    delta: impl Read,
    base: impl Read,
    result: impl Read,
    temp_dir: &Path,
) -> Result<()> {
    let mut json = Json {
        r: BufReader::new(delta),
    };
    let mut base = BufReader::new(base);
    let mut output = Compare { expected: result };
    let mut cursor = 0u64;
    let mut previous = None;
    let mut seen = BTreeSet::new();
    json.expect(b'{')?;
    loop {
        if json.peek()? == Some(b'}') {
            json.byte()?;
            break;
        }
        let key = json.small()?;
        if !seen.insert(key.clone()) {
            return Err(Error::Corrupt("duplicate delta field".into()));
        }
        json.expect(b':')?;
        match key.as_str() {
            "codec" => {
                if json.small()? != "utf8-splice-v1" {
                    return Err(Error::Corrupt("delta codec".into()));
                }
            }
            "edits" => {
                json.expect(b'[')?;
                loop {
                    if json.peek()? == Some(b']') {
                        json.byte()?;
                        break;
                    }
                    json.expect(b'{')?;
                    let mut start = None;
                    let mut delete = None;
                    let mut keys = BTreeSet::new();
                    let path = temp_dir.join(format!("delta-{}.tmp", uuid::Uuid::new_v4()));
                    let mut insertion = OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .read(true)
                        .open(&path)?;
                    loop {
                        if json.peek()? == Some(b'}') {
                            json.byte()?;
                            break;
                        }
                        let k = json.small()?;
                        if !keys.insert(k.clone()) {
                            return Err(Error::Corrupt("duplicate edit key".into()));
                        }
                        json.expect(b':')?;
                        match k.as_str() {
                            "start_byte" => start = Some(json.number_string()?),
                            "delete_bytes" => delete = Some(json.number_string()?),
                            "insert_utf8" => json.string(&mut insertion)?,
                            _ => return Err(Error::Corrupt("unknown edit key".into())),
                        }
                        match json.peek()? {
                            Some(b',') => {
                                json.byte()?;
                                if json.peek()? == Some(b'}') {
                                    return Err(Error::Corrupt("trailing edit comma".into()));
                                }
                            }
                            Some(b'}') => {}
                            _ => return Err(Error::Corrupt("edit object separator".into())),
                        }
                    }
                    if keys.len() != 3 {
                        return Err(Error::Corrupt("incomplete edit".into()));
                    }
                    let start = start.ok_or_else(|| Error::Corrupt("missing start".into()))?;
                    let delete = delete.ok_or_else(|| Error::Corrupt("missing delete".into()))?;
                    if start < cursor || previous == Some(start) {
                        return Err(Error::Corrupt(
                            "overlapping/duplicate-position edits".into(),
                        ));
                    }
                    let unchanged = start - cursor;
                    if std::io::copy(&mut Read::by_ref(&mut base).take(unchanged), &mut output)?
                        != unchanged
                    {
                        return Err(Error::Corrupt("splice start outside base".into()));
                    }
                    boundary(&mut base)?;
                    if std::io::copy(
                        &mut Read::by_ref(&mut base).take(delete),
                        &mut std::io::sink(),
                    )? != delete
                    {
                        return Err(Error::Corrupt("splice delete outside base".into()));
                    }
                    boundary(&mut base)?;
                    insertion.seek(SeekFrom::Start(0))?;
                    std::io::copy(&mut insertion, &mut output)?;
                    drop(insertion);
                    fs::remove_file(path)?;
                    cursor = start.checked_add(delete).ok_or(Error::Capacity)?;
                    previous = Some(start);
                    match json.peek()? {
                        Some(b',') => {
                            json.byte()?;
                            if json.peek()? == Some(b']') {
                                return Err(Error::Corrupt("trailing edits comma".into()));
                            }
                        }
                        Some(b']') => {}
                        _ => return Err(Error::Corrupt("edits separator".into())),
                    }
                }
            }
            _ => return Err(Error::Corrupt("unknown delta field".into())),
        }
        match json.peek()? {
            Some(b',') => {
                json.byte()?;
                if json.peek()? == Some(b'}') {
                    return Err(Error::Corrupt("trailing delta comma".into()));
                }
            }
            Some(b'}') => {}
            _ => return Err(Error::Corrupt("delta object separator".into())),
        }
    }
    if seen.len() != 2
        || !seen.contains("codec")
        || !seen.contains("edits")
        || json.peek()?.is_some()
    {
        return Err(Error::Corrupt("incomplete/trailing delta JSON".into()));
    }
    std::io::copy(&mut base, &mut output)?;
    let mut tail = [0];
    if output.expected.read(&mut tail)? != 0 {
        return Err(Error::Corrupt("delta result too short".into()));
    }
    Ok(())
}
