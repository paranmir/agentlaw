use super::*;
use std::io::{BufReader, Read};

#[test]
fn invalid_payload_utf8_is_corrupt() {
    let frame = Frame {
        kind: "body".into(),
        key: "22222222-2222-4222-8222-222222222222".into(),
        payload: b"valid".to_vec(),
    };
    let mut bytes = Vec::new();
    write("current", &[frame], &mut bytes).unwrap();
    let pos = bytes.windows(5).position(|part| part == b"valid").unwrap();
    bytes[pos] = 0xff;
    assert!(matches!(
        scan(
            "current",
            &mut BufReader::new(bytes.as_slice()),
            |_, payload| {
                std::io::copy(payload, &mut std::io::sink())?;
                Ok(())
            }
        ),
        Err(Error::Corrupt(_))
    ));
}

struct FailingReader {
    bytes: Vec<u8>,
    position: usize,
    fail_at: usize,
    kind: std::io::ErrorKind,
}
impl Read for FailingReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.fail_at {
            return Err(std::io::Error::new(self.kind, "injected reader failure"));
        }
        let count = out
            .len()
            .min(self.fail_at - self.position)
            .min(self.bytes.len() - self.position);
        out[..count].copy_from_slice(&self.bytes[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

#[test]
fn actual_payload_reader_errors_remain_io_errors() {
    let body = "real reader error must not become corruption";
    let unit = crate::CurrentUnit {
        entity_id: "11111111-1111-4111-8111-111111111111".into(),
        entity_type: "memory".into(),
        state: crate::UnitState::Live {
            heads: vec![crate::Head {
                metadata: serde_json::json!({"change_id":"22222222-2222-4222-8222-222222222222","applicability":{"scope":"user"},"origin":{"machine_id":"33333333-3333-4333-8333-333333333333"},"recorded_at_ms":0,"is_rule":false,"relations":[],"work_targets":[]}),
                body: body.into(),
            }],
        },
    };
    let bytes = unit.encode().unwrap();
    let body_start = bytes
        .windows(body.len())
        .position(|window| window == body.as_bytes())
        .unwrap();
    for kind in [
        std::io::ErrorKind::InvalidData,
        std::io::ErrorKind::UnexpectedEof,
        std::io::ErrorKind::PermissionDenied,
    ] {
        for invalid_prefix in [false, true] {
            let mut input = bytes.clone();
            if invalid_prefix {
                input[body_start] = 0xff;
            }
            let reader = || {
                BufReader::with_capacity(
                    16,
                    FailingReader {
                        bytes: input.clone(),
                        position: 0,
                        fail_at: body_start + 5,
                        kind,
                    },
                )
            };
            let old = crate::CurrentUnit::decode(&mut reader());
            let compact = read_current_owned(&mut reader(), u64::MAX);
            assert!(matches!(old, Err(Error::Io(ref error)) if error.kind() == kind));
            assert!(matches!(compact, Err(Error::Io(ref error)) if error.kind() == kind));
        }
    }
}
