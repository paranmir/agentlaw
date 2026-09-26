//! Length-delimited Markdown codec. Payload markers never delimit records.
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{BufRead, Read, Write},
};

pub const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct FrameInfo {
    pub kind: String,
    pub key: String,
    pub bytes: u64,
    pub sha256: String,
    pub payload_offset: u64,
}

/// Streaming framed read: the consumer may copy or skip a payload. The scanner
/// drains and verifies every byte, so skipping never bypasses integrity checks.
pub fn scan(
    kind: &str,
    r: &mut impl BufRead,
    mut consume: impl FnMut(&FrameInfo, &mut dyn Read) -> Result<()>,
) -> Result<u64> {
    let first = line(r)?;
    if first != format!("<!-- agentlaw-file-v1 kind={kind} -->\n") {
        return Err(Error::Corrupt("unsupported file header".into()));
    }
    let mut offset = first.len() as u64;
    let mut records = 0u64;
    let mut seen = BTreeSet::new();
    loop {
        let header = line(r)?;
        offset = offset
            .checked_add(header.len() as u64)
            .ok_or(Error::Capacity)?;
        if let Some(n) = header
            .strip_prefix("<!-- /agentlaw-file-v1 records=")
            .and_then(|s| s.strip_suffix(" -->\n"))
        {
            if count(n)? != records || !r.fill_buf()?.is_empty() {
                return Err(Error::Corrupt("trailer count/trailing data".into()));
            }
            return Ok(records);
        }
        let fields = header
            .strip_prefix("<!-- agentlaw-record-v1 ")
            .and_then(|s| s.strip_suffix(" -->\n"))
            .ok_or_else(|| Error::Corrupt("frame header".into()))?
            .split(' ')
            .collect::<Vec<_>>();
        if fields.len() != 4 {
            return Err(Error::Corrupt("frame field count".into()));
        }
        let ty = fields[0]
            .strip_prefix("type=")
            .ok_or_else(|| Error::Corrupt("frame type".into()))?;
        let key = fields[1]
            .strip_prefix("key=")
            .ok_or_else(|| Error::Corrupt("frame key".into()))?;
        let bytes = count(
            fields[2]
                .strip_prefix("bytes=")
                .ok_or_else(|| Error::Corrupt("frame bytes".into()))?,
        )?;
        let sha = fields[3]
            .strip_prefix("sha256=")
            .ok_or_else(|| Error::Corrupt("frame hash".into()))?;
        if !registered(kind, ty) || !seen.insert((ty.to_owned(), key.to_owned())) {
            return Err(Error::Corrupt("unknown/duplicate frame".into()));
        }
        if key == "root" {
            if kind != "format" {
                return Err(Error::Corrupt("reserved root key".into()));
            }
        } else {
            let mut p = key.split('.');
            crate::validate_id(p.next().unwrap())?;
            if let Some(n) = p.next() {
                if count(n)? > u32::MAX as u64 || p.next().is_some() {
                    return Err(Error::Corrupt("chunk key".into()));
                }
            }
        }
        let end = offset
            .checked_add(bytes)
            .and_then(|n| n.checked_add(30))
            .ok_or(Error::Capacity)?;
        if kind == "history" && (bytes > 1024 * 1024 || end > 16 * 1024 * 1024) {
            return Err(Error::Capacity);
        }
        let info = FrameInfo {
            kind: ty.into(),
            key: key.into(),
            bytes,
            sha256: sha.into(),
            payload_offset: offset,
        };
        let mut payload = CheckedPayload {
            inner: r.take(bytes),
            hash: Sha256::new(),
            count: 0,
            tail: Vec::new(),
        };
        consume(&info, &mut payload)?;
        std::io::copy(&mut payload, &mut std::io::sink())?;
        if payload.count != bytes
            || !payload.tail.is_empty()
            || format!("{:x}", payload.hash.finalize()) != sha
        {
            return Err(Error::Corrupt("payload length/UTF8/digest".into()));
        }
        let mut footer = [0u8; 30];
        r.read_exact(&mut footer)?;
        if &footer != b"\n<!-- /agentlaw-record-v1 -->\n" {
            return Err(Error::Corrupt("frame footer".into()));
        }
        offset = end;
        records = records.checked_add(1).ok_or(Error::Capacity)?;
    }
}
struct CheckedPayload<R> {
    inner: R,
    hash: Sha256,
    count: u64,
    tail: Vec<u8>,
}
impl<R: Read> Read for CheckedPayload<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(out)?;
        if n == 0 {
            return Ok(0);
        }
        self.hash.update(&out[..n]);
        self.count = self
            .count
            .checked_add(n as u64)
            .ok_or_else(|| std::io::Error::other("byte count overflow"))?;
        let mut bytes = std::mem::take(&mut self.tail);
        bytes.extend_from_slice(&out[..n]);
        match std::str::from_utf8(&bytes) {
            Ok(_) => {}
            Err(e) => {
                if e.error_len().is_some() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid UTF8 payload",
                    ));
                }
                self.tail = bytes[e.valid_up_to()..].to_vec();
            }
        }
        Ok(n)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: String,
    pub key: String,
    pub payload: Vec<u8>,
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn line(r: &mut impl BufRead) -> Result<String> {
    let mut b = Vec::new();
    r.take(513).read_until(b'\n', &mut b)?;
    if b.len() > 512 || b.last() != Some(&b'\n') || !b.is_ascii() {
        return Err(Error::Corrupt("header length/encoding".into()));
    }
    String::from_utf8(b).map_err(|_| Error::Corrupt("header UTF-8".into()))
}
fn count(s: &str) -> Result<u64> {
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Error::Corrupt("noncanonical decimal".into()));
    }
    s.parse()
        .map_err(|_| Error::Corrupt("length overflow".into()))
}
fn registered(kind: &str, ty: &str) -> bool {
    match kind {
        "current" => matches!(ty, "state" | "body" | "instructions"),
        "history" => matches!(
            ty,
            "change_descriptor"
                | "change_metadata_chunk"
                | "delta_chunk"
                | "evidence_chunk"
                | "checkpoint_descriptor"
                | "checkpoint_metadata_chunk"
                | "checkpoint_body_chunk"
        ),
        "catalog" => ty == "project",
        "format" => ty == "format",
        _ => false,
    }
}
pub fn write(kind: &str, frames: &[Frame], w: &mut impl Write) -> Result<()> {
    writeln!(w, "<!-- agentlaw-file-v1 kind={kind} -->")?;
    let mut seen = BTreeSet::new();
    for f in frames {
        if !registered(kind, &f.kind)
            || !seen.insert((&f.kind, &f.key))
            || f.key.contains(char::is_whitespace)
        {
            return Err(Error::Corrupt("invalid frame identity".into()));
        }
        writeln!(
            w,
            "<!-- agentlaw-record-v1 type={} key={} bytes={} sha256={} -->",
            f.kind,
            f.key,
            f.payload.len(),
            digest(&f.payload)
        )?;
        w.write_all(&f.payload)?;
        w.write_all(b"\n<!-- /agentlaw-record-v1 -->\n")?;
    }
    writeln!(w, "<!-- /agentlaw-file-v1 records={} -->", frames.len())?;
    Ok(())
}
/// Per-frame bounded allocation; does not allocate from an unchecked on-disk length.
pub fn read(kind: &str, r: &mut impl BufRead) -> Result<Vec<Frame>> {
    if line(r)? != format!("<!-- agentlaw-file-v1 kind={kind} -->\n") {
        return Err(Error::Corrupt("unsupported file header".into()));
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    loop {
        let header = line(r)?;
        if let Some(n) = header
            .strip_prefix("<!-- /agentlaw-file-v1 records=")
            .and_then(|s| s.strip_suffix(" -->\n"))
        {
            if count(n)? != out.len() as u64 || !r.fill_buf()?.is_empty() {
                return Err(Error::Corrupt("trailer count/trailing data".into()));
            }
            return Ok(out);
        }
        let fields: Vec<_> = header
            .strip_prefix("<!-- agentlaw-record-v1 ")
            .and_then(|s| s.strip_suffix(" -->\n"))
            .ok_or_else(|| Error::Corrupt("record header".into()))?
            .split(' ')
            .collect();
        if fields.len() != 4 {
            return Err(Error::Corrupt("record fields".into()));
        }
        let ty = fields[0]
            .strip_prefix("type=")
            .ok_or_else(|| Error::Corrupt("type".into()))?;
        let key = fields[1]
            .strip_prefix("key=")
            .ok_or_else(|| Error::Corrupt("key".into()))?;
        let n = count(
            fields[2]
                .strip_prefix("bytes=")
                .ok_or_else(|| Error::Corrupt("bytes".into()))?,
        )?;
        let hash = fields[3]
            .strip_prefix("sha256=")
            .ok_or_else(|| Error::Corrupt("sha256".into()))?;
        if !registered(kind, ty) || !seen.insert((ty.to_owned(), key.to_owned())) {
            return Err(Error::Corrupt("unknown or duplicate frame".into()));
        }
        if key != "root" {
            let mut parts = key.split('.');
            crate::validate_id(parts.next().unwrap())?;
            if let Some(chunk) = parts.next() {
                let c = count(chunk)?;
                if c > u32::MAX as u64 || parts.next().is_some() {
                    return Err(Error::Corrupt("chunk key".into()));
                }
            }
        }
        total = total.checked_add(n).ok_or(Error::Capacity)?;
        if n > MAX_FRAME_BYTES || total > MAX_FRAME_BYTES {
            return Err(Error::Capacity);
        }
        let mut payload = Vec::new();
        r.take(n).read_to_end(&mut payload)?;
        if payload.len() as u64 != n
            || digest(&payload) != hash
            || std::str::from_utf8(&payload).is_err()
        {
            return Err(Error::Corrupt("payload length/digest/UTF-8".into()));
        }
        let mut footer = [0; 30];
        r.read_exact(&mut footer)?;
        if &footer != b"\n<!-- /agentlaw-record-v1 -->\n" {
            return Err(Error::Corrupt("record footer".into()));
        }
        out.push(Frame {
            kind: ty.into(),
            key: key.into(),
            payload,
        });
    }
}

/// Canonical JSON uses explicit control-character escapes, unlike serde's short escapes.
pub fn canonical_json(v: &serde_json::Value) -> Result<Vec<u8>> {
    fn enc(v: &serde_json::Value, s: &mut String) -> Result<()> {
        use serde_json::Value::*;
        match v {
            Null => s.push_str("null"),
            Bool(x) => s.push_str(if *x { "true" } else { "false" }),
            Number(n) => {
                if n.is_f64() {
                    return Err(Error::Corrupt("floating metadata number".into()));
                }
                s.push_str(&n.to_string());
            }
            String(x) => {
                s.push('"');
                for c in x.chars() {
                    match c {
                        '"' => s.push_str("\\\""),
                        '\\' => s.push_str("\\\\"),
                        '\u{0}'..='\u{1f}' => s.push_str(&format!("\\u{:04x}", c as u32)),
                        _ => s.push(c),
                    }
                }
                s.push('"');
            }
            Array(a) => {
                s.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        s.push(',')
                    }
                    enc(x, s)?;
                }
                s.push(']');
            }
            Object(o) => {
                s.push('{');
                let sorted: std::collections::BTreeMap<_, _> = o.iter().collect();
                for (i, (k, x)) in sorted.iter().enumerate() {
                    if i > 0 {
                        s.push(',')
                    }
                    enc(&String((*k).clone()), s)?;
                    s.push(':');
                    enc(x, s)?;
                }
                s.push('}');
            }
        }
        Ok(())
    }
    let mut s = String::new();
    enc(v, &mut s)?;
    Ok(s.into_bytes())
}

/// serde_json::Value alone silently accepts duplicate keys. Canonical inputs do not.
pub fn parse_json(bytes: &[u8]) -> Result<serde_json::Value> {
    use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
    struct Strict(serde_json::Value);
    impl<'de> Deserialize<'de> for Strict {
        fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Strict;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("unique-key JSON with integer numbers")
                }
                fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Strict, E> {
                    Err(E::custom("floating number rejected"))
                }
                fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_none<E: de::Error>(self) -> std::result::Result<Strict, E> {
                    Ok(Strict(serde_json::Value::Null))
                }
                fn visit_unit<E: de::Error>(self) -> std::result::Result<Strict, E> {
                    Ok(Strict(serde_json::Value::Null))
                }
                fn visit_seq<A: SeqAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Strict, A::Error> {
                    let mut v = Vec::new();
                    while let Some(x) = a.next_element::<Strict>()? {
                        v.push(x.0)
                    }
                    Ok(Strict(v.into()))
                }
                fn visit_map<A: MapAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Strict, A::Error> {
                    let mut m = serde_json::Map::new();
                    while let Some((k, v)) = a.next_entry::<String, Strict>()? {
                        if m.insert(k, v.0).is_some() {
                            return Err(de::Error::custom("duplicate key"));
                        }
                    }
                    Ok(Strict(m.into()))
                }
            }
            d.deserialize_any(V)
        }
    }
    Ok(serde_json::from_slice::<Strict>(bytes)?.0)
}
