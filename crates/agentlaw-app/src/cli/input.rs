//! Explicit payload reads, invoked only after pure CLI argument validation.
use agentlaw_contracts::{DomainError, Result};
use agentlaw_storage::import_conflict::ImportSide;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    path::Path,
};

pub(crate) fn read_stdin() -> Result<String> {
    read_bounded(io::stdin().lock(), InputSource::Stdin)
}

pub(crate) fn read_path_or_stdin(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        return read_stdin();
    }
    read_file(path)
}

pub(crate) fn read_file(path: &Path) -> Result<String> {
    let file = File::open(path).map_err(|_| {
        DomainError::new(
            "input_unreadable",
            "Cannot read the input file; no partial request was executed.",
        )
    })?;
    read_bounded(file, InputSource::File)
}

pub(crate) fn import_choices(path: &Path) -> Result<BTreeMap<String, ImportSide>> {
    let text = if path == Path::new("-") {
        read_stdin()?
    } else {
        let file = File::open(path).map_err(|_| {
            DomainError::new(
                "input_unreadable",
                "Cannot read the structural choice file; no import was modified.",
            )
        })?;
        read_bounded(file, InputSource::StructuralChoices)?
    };
    decode_import_choices(&text)
}

#[derive(Clone, Copy)]
enum InputSource {
    Stdin,
    File,
    StructuralChoices,
}

fn read_bounded(reader: impl Read, source: InputSource) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take((agentlaw_app::MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| match source {
            InputSource::Stdin => DomainError::new("transport_io", "Could not read stdin."),
            InputSource::File => {
                DomainError::new("input_unreadable", "Cannot read the complete input file.")
            }
            InputSource::StructuralChoices => DomainError::new(
                "input_unreadable",
                "Cannot read the complete structural choice file.",
            ),
        })?;
    if bytes.len() > agentlaw_app::MAX_REQUEST_BYTES {
        let message = match source {
            InputSource::Stdin | InputSource::File => {
                "CLI input exceeds 16 MiB; no partial request was executed."
            }
            InputSource::StructuralChoices => {
                "Structural choices exceed 16 MiB; no partial choice was applied."
            }
        };
        return Err(DomainError::new("transport_capacity", message));
    }
    String::from_utf8(bytes).map_err(|_| {
        let message = match source {
            InputSource::Stdin | InputSource::File => "Expected UTF-8 JSON input.",
            InputSource::StructuralChoices => "Structural choices must be UTF-8 JSON.",
        };
        DomainError::new("invalid_encoding", message)
    })
}

fn decode_import_choices(text: &str) -> Result<BTreeMap<String, ImportSide>> {
    let value = agentlaw_contracts::validation::decode_unique(text)?;
    let choices: BTreeMap<String, ImportSide> = serde_json::from_value(value).map_err(|_| {
        DomainError::new(
            "invalid_arguments",
            "Expected an object mapping returned conflict_id values to \"local\" or \"incoming\". Explain both states and obtain the user's choice first.",
        )
    })?;
    if choices.is_empty() || choices.keys().any(|id| uuid::Uuid::parse_str(id).is_err()) {
        return Err(DomainError::new(
            "invalid_arguments",
            "Provide at least one returned conflict_id and an explicit local/incoming choice. Do not invent conflict identifiers.",
        ));
    }
    Ok(choices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const CONFLICT_ID: &str = "fa6f71f9-97bc-4a89-9c96-21f3d88ac2d5";

    #[test]
    fn accepts_utf8_up_to_the_byte_limit() {
        let mut bytes = vec![b' '; agentlaw_app::MAX_REQUEST_BYTES - 3];
        bytes.extend_from_slice("한".as_bytes());
        let text = read_bounded(Cursor::new(&bytes), InputSource::Stdin).unwrap();
        assert_eq!(text.len(), agentlaw_app::MAX_REQUEST_BYTES);
        assert!(text.ends_with('한'));
    }

    #[test]
    fn stops_at_limit_plus_one_for_every_input_source() {
        let bytes = vec![b' '; agentlaw_app::MAX_REQUEST_BYTES + 4];
        for source in [
            InputSource::Stdin,
            InputSource::File,
            InputSource::StructuralChoices,
        ] {
            let mut reader = Cursor::new(&bytes);
            let error = read_bounded(&mut reader, source).unwrap_err();
            assert_eq!(error.code, "transport_capacity");
            assert_eq!(
                reader.position(),
                (agentlaw_app::MAX_REQUEST_BYTES + 1) as u64
            );
        }
    }

    #[test]
    fn rejects_invalid_utf8_without_echoing_input() {
        for source in [
            InputSource::Stdin,
            InputSource::File,
            InputSource::StructuralChoices,
        ] {
            let error = read_bounded(Cursor::new([b's', b'e', b'c', 0xff]), source).unwrap_err();
            assert_eq!(error.code, "invalid_encoding");
            assert!(!error.message.contains("sec"));
        }
    }

    #[test]
    fn preserves_read_error_codes() {
        struct Unreadable;
        impl Read for Unreadable {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("private reader detail"))
            }
        }
        for (source, code) in [
            (InputSource::Stdin, "transport_io"),
            (InputSource::File, "input_unreadable"),
            (InputSource::StructuralChoices, "input_unreadable"),
        ] {
            let error = read_bounded(Unreadable, source).unwrap_err();
            assert_eq!(error.code, code);
            assert!(!error.message.contains("private reader detail"));
        }
    }

    #[test]
    fn rejects_a_valid_partial_payload_when_a_later_read_fails() {
        struct PrefixThenFailure<'a> {
            prefix: Cursor<&'a [u8]>,
            read_calls: usize,
        }
        impl Read for PrefixThenFailure<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                assert!(bytes.len() <= agentlaw_app::MAX_REQUEST_BYTES + 1);
                self.read_calls += 1;
                if self.prefix.position() < self.prefix.get_ref().len() as u64 {
                    self.prefix.read(bytes)
                } else {
                    Err(io::Error::other("private reader detail"))
                }
            }
        }

        let prefix = br#"{"secret":"partial-input-secret"}"#;
        assert!(serde_json::from_slice::<serde_json::Value>(prefix).is_ok());
        for (source, code) in [
            (InputSource::Stdin, "transport_io"),
            (InputSource::File, "input_unreadable"),
            (InputSource::StructuralChoices, "input_unreadable"),
        ] {
            let mut reader = PrefixThenFailure {
                prefix: Cursor::new(prefix.as_slice()),
                read_calls: 0,
            };
            let error = read_bounded(&mut reader, source).unwrap_err();
            assert_eq!(error.code, code);
            assert_eq!(reader.prefix.position(), prefix.len() as u64);
            assert!(reader.prefix.position() <= (agentlaw_app::MAX_REQUEST_BYTES + 1) as u64);
            assert!(reader.read_calls >= 2);
            assert!(!error.message.contains("partial-input-secret"));
            assert!(!error.message.contains("private reader detail"));
        }
    }

    #[test]
    fn reads_explicit_file_without_parsing_its_payload() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "한글 payload").unwrap();
        assert_eq!(read_path_or_stdin(file.path()).unwrap(), "한글 payload");
    }

    #[test]
    fn reads_dash_named_file_as_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("-");
        std::fs::write(&path, "dash-named file").unwrap();
        assert_eq!(read_file(&path).unwrap(), "dash-named file");
    }

    #[test]
    fn reports_unreadable_files_without_echoing_the_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private-missing-input.json");
        for error in [
            read_path_or_stdin(&path).unwrap_err(),
            import_choices(&path).unwrap_err(),
        ] {
            assert_eq!(error.code, "input_unreadable");
            assert!(!error.message.contains("private-missing-input"));
        }
    }

    #[test]
    fn reads_structural_choices_from_an_explicit_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), format!(r#"{{"{CONFLICT_ID}":"incoming"}}"#)).unwrap();
        let choices = import_choices(file.path()).unwrap();
        assert_eq!(choices.get(CONFLICT_ID), Some(&ImportSide::Incoming));
    }

    #[test]
    fn accepts_explicit_local_and_incoming_choices() {
        let other_id = "ea7d4f0e-032d-40b9-aa3c-a72d39ba4d54";
        let text = format!(r#"{{"{CONFLICT_ID}":"local","{other_id}":"incoming"}}"#);
        let choices = decode_import_choices(&text).unwrap();
        assert_eq!(choices.get(CONFLICT_ID), Some(&ImportSide::Local));
        assert_eq!(choices.get(other_id), Some(&ImportSide::Incoming));
    }

    #[test]
    fn rejects_duplicate_keys_before_choice_decoding() {
        let text = format!(r#"{{"{CONFLICT_ID}":"local","{CONFLICT_ID}":"incoming"}}"#);
        assert_eq!(
            decode_import_choices(&text).unwrap_err().code,
            "invalid_input"
        );
    }

    #[test]
    fn rejects_malformed_json() {
        assert_eq!(
            decode_import_choices("{not JSON}").unwrap_err().code,
            "invalid_input"
        );
    }

    #[test]
    fn requires_nonempty_uuid_keys_and_supported_choices() {
        for text in [
            "{}".to_owned(),
            r#"{"invented":"local"}"#.to_owned(),
            format!(r#"{{"{CONFLICT_ID}":"both"}}"#),
            format!(r#"{{"{CONFLICT_ID}":null}}"#),
            "[]".to_owned(),
        ] {
            assert_eq!(
                decode_import_choices(&text).unwrap_err().code,
                "invalid_arguments"
            );
        }
    }
}
