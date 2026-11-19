//! Conversion of pinned current units into deterministic embedding pieces.

use agentlaw_contracts::{Applicability, ScopeKind};
use agentlaw_search::vector_file::CurrentHeads;
use std::io::{BufReader, Read};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Piece {
    pub memory_id: String,
    pub change_id: String,
    pub section: String,
    pub scope: String,
    pub text: String,
}

struct StreamSource {
    memory_id: String,
    change_id: String,
    section: &'static str,
    scope: String,
    reader: BufReader<Box<dyn Read + Send>>,
    // Keeps the acquired immutable spool alive while its reader is open.
    _lease: Option<agentlaw_storage::acquire::SpoolBody>,
}

pub(super) struct AcquiredPieces {
    streams: std::vec::IntoIter<StreamSource>,
    current: Option<StreamSource>,
    windows: Option<Box<dyn Iterator<Item = std::io::Result<String>> + Send>>,
    section: usize,
}

pub(super) fn history_pieces(
    change: agentlaw_storage::history_spool::SpoolChange,
) -> Result<AcquiredPieces, String> {
    let change_id = change.metadata["change_id"]
        .as_str()
        .ok_or("missing history change identity")?
        .to_owned();
    let body = change.body.open().map_err(|e| e.to_string())?;
    let evidence = change.evidence.open().map_err(|e| e.to_string())?;
    let reader = body
        .chain(std::io::Cursor::new(b"\n".to_vec()))
        .chain(evidence);
    let stream = StreamSource {
        memory_id: format!("change:{change_id}"),
        change_id,
        section: "history",
        scope: "history".into(),
        reader: BufReader::new(Box::new(reader)),
        _lease: None,
    };
    Ok(AcquiredPieces {
        streams: vec![stream].into_iter(),
        current: None,
        windows: None,
        section: 0,
    })
}

pub(super) fn map_acquired(
    current: agentlaw_storage::acquire::AcquiredCurrent,
    id: &str,
    procedure: bool,
) -> Result<(CurrentHeads, AcquiredPieces), String> {
    let memory_id = if procedure {
        format!("procedure/{id}")
    } else {
        id.to_owned()
    };
    let mut heads = CurrentHeads {
        memory_id: memory_id.clone(),
        change_ids: Vec::new(),
    };
    let mut streams = Vec::new();
    if current.state["state"] != "redirect" {
        let metadata = current.state["current_heads"]
            .as_array()
            .ok_or("missing current heads")?;
        if metadata.len() != current.references.len() {
            return Err("acquired head references mismatch".into());
        }
        for (meta, reference) in metadata.iter().zip(&current.references) {
            let change_id = reference.observed_version.clone();
            heads.change_ids.push(change_id.clone());
            let mut scope = scope_token(&meta["applicability"])?;
            let (section, reader, lease): (&'static str, Box<dyn Read + Send>, _) = if procedure {
                scope = format!("procedure:{scope}");
                let name = meta["name"].as_str().ok_or("missing procedure name")?;
                let when = meta["use_when"]
                    .as_str()
                    .ok_or("missing procedure use_when")?;
                (
                    "descriptor",
                    Box::new(std::io::Cursor::new(format!("{name}\n{when}").into_bytes())),
                    None,
                )
            } else {
                let change = meta["change_id"].as_str().ok_or("missing body change")?;
                let body = current
                    .bodies
                    .get(change)
                    .ok_or("missing acquired body")?
                    .clone();
                let file = body.open().map_err(|e| e.to_string())?;
                ("body", Box::new(file), Some(body))
            };
            streams.push(StreamSource {
                memory_id: memory_id.clone(),
                change_id,
                section,
                scope,
                reader: BufReader::new(reader),
                _lease: lease,
            });
        }
    }
    Ok((
        heads,
        AcquiredPieces {
            streams: streams.into_iter(),
            current: None,
            windows: None,

            section: 0,
        },
    ))
}

impl Iterator for AcquiredPieces {
    type Item = Result<Piece, String>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.current.is_none() {
            self.current = self.streams.next();
            self.section = 0;
        }
        let stream = self.current.as_mut()?;
        let windows = self.windows.get_or_insert_with(|| {
            let reader = std::mem::replace(
                &mut stream.reader,
                BufReader::new(Box::new(std::io::empty())),
            );
            Box::new(super::pieces::read_windows(reader))
        });
        if let Some(text) = windows.next() {
            let section = format!("{}:{}", stream.section, self.section);
            self.section += 1;
            return Some(
                text.map(|text| Piece {
                    memory_id: stream.memory_id.clone(),
                    change_id: stream.change_id.clone(),
                    section,
                    scope: stream.scope.clone(),
                    text,
                })
                .map_err(|e| e.to_string()),
            );
        }
        self.current = None;
        self.windows = None;
        self.next()
    }
}
fn scope_token(value: &serde_json::Value) -> Result<String, String> {
    let mut value = value.clone();
    let scope = value["scope"].as_str().ok_or("missing scope")?;
    value["scope"] = match scope {
        "project_machine" => serde_json::json!(["project", "machine"]),
        "user" | "project" | "machine" => serde_json::json!([scope]),
        _ => return Err("unknown scope".into()),
    };
    let applicability: Applicability = serde_json::from_value(value).map_err(|e| e.to_string())?;
    applicability.validate().map_err(|e| e.to_string())?;
    for id in applicability
        .project_id
        .iter()
        .chain(applicability.machine_id.iter())
    {
        agentlaw_storage::validate_id(id).map_err(|e| e.to_string())?;
    }
    Ok(match applicability.scope.as_slice() {
        [ScopeKind::User] => "user".into(),
        [ScopeKind::Machine] => format!("machine:{}", applicability.machine_id.unwrap()),
        [ScopeKind::Project] => format!("project:{}", applicability.project_id.unwrap()),
        [ScopeKind::Project, ScopeKind::Machine] => format!(
            "project:{}:machine:{}",
            applicability.project_id.unwrap(),
            applicability.machine_id.unwrap()
        ),
        _ => return Err("invalid scope".into()),
    })
}
