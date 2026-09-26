//! C6-to-C7 read-only adapter. This value does not expose any source publication API.
use agentlaw_storage::published::PublishedChangeSource;
use agentlaw_storage::UnitState;
use agentlaw_worker::derived::*;
pub struct PublishedAdapter<R> {
    reader: R,
    binding: String,
}
impl<R: PublishedChangeSource> PublishedAdapter<R> {
    pub fn new(reader: R, binding: String) -> Self {
        Self { reader, binding }
    }
    pub fn position(&self) -> Result<SourcePosition> {
        let p = self.reader.position().map_err(map_error)?;
        Ok(SourcePosition {
            epoch: p.epoch,
            sequence: p.sequence,
        })
    }
}
impl PublishedAdapter<agentlaw_storage::published::OwnedPublishedReader> {
    fn stage_paths(
        &self,
        worker: &agentlaw_worker::process::Client,
        canonical_paths: &[String],
        position: &agentlaw_storage::published::SourcePosition,
    ) -> Result<(
        Vec<DerivedDocument>,
        Vec<agentlaw_worker::spool::SpoolBodyRef>,
    )> {
        use agentlaw_worker::spool::SpoolBodyRef;
        let mut documents = Vec::new();
        let mut bodies = Vec::new();
        for path in canonical_paths {
            let procedure = path.starts_with("current/procedure/");
            if !procedure && !path.starts_with("current/memory/") {
                continue;
            }
            let id = std::path::Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| Error::SourceUnavailable("Invalid current identity path.".into()))?;
            let current = if procedure {
                self.reader.acquire_procedure(id)
            } else {
                self.reader.acquire_current(id)
            }
            .map_err(map_error)?;
            if current.source_position != *position {
                return Err(Error::SourceUnavailable("Source changed while acquiring published bodies; retry without advancing cursor.".into()));
            }
            if current.state["state"] == "redirect" {
                documents.push(DerivedDocument {
                    memory_id: id.into(),
                    change_id: current.state["consolidation_change_id"]
                        .as_str()
                        .ok_or_else(|| Error::SourceUnavailable("Missing redirect change.".into()))?
                        .into(),
                    scope: String::new(),
                    body: None,
                    section: "redirect".into(),
                });
                continue;
            }
            for (index, metadata) in current.state["current_heads"]
                .as_array()
                .ok_or_else(|| Error::SourceUnavailable("Missing current heads.".into()))?
                .iter()
                .enumerate()
            {
                let reference = current
                    .references
                    .get(index)
                    .ok_or_else(|| Error::SourceUnavailable("Missing acquired version.".into()))?;
                let applicability = crate::runtime::scope_value(&metadata["applicability"])
                    .map_err(|_| Error::SourceUnavailable("Invalid published scope.".into()))?;
                let scope = crate::runtime::scope_token(&applicability);
                let document_index = documents.len();
                let body = if procedure {
                    let descriptor = format!(
                        "{}\n{}",
                        metadata["name"]
                            .as_str()
                            .ok_or_else(|| Error::SourceUnavailable(
                                "Missing procedure name.".into()
                            ))?,
                        metadata["use_when"].as_str().ok_or_else(|| {
                            Error::SourceUnavailable("Missing procedure use_when.".into())
                        })?
                    );
                    worker
                        .stage_source_body(&mut descriptor.as_bytes())
                        .map_err(|_| {
                            Error::SourceUnavailable("Cannot stage procedure descriptor.".into())
                        })?
                } else {
                    let change = metadata["change_id"]
                        .as_str()
                        .ok_or_else(|| Error::SourceUnavailable("Missing body change.".into()))?;
                    let source = current
                        .bodies
                        .get(change)
                        .ok_or_else(|| Error::SourceUnavailable("Missing acquired body.".into()))?;
                    let staged = worker
                        .stage_source_body(&mut source.open().map_err(map_error)?)
                        .map_err(|_| {
                            Error::SourceUnavailable(
                                "Cannot stage immutable body for worker.".into(),
                            )
                        })?;
                    if staged.bytes != source.bytes || staged.sha256 != source.sha256 {
                        return Err(Error::SourceUnavailable(
                            "Acquired body changed during transfer.".into(),
                        ));
                    }
                    staged
                };
                documents.push(DerivedDocument {
                    memory_id: if procedure {
                        format!("procedure/{id}")
                    } else {
                        id.into()
                    },
                    change_id: reference.observed_version.clone(),
                    scope: if procedure {
                        format!("procedure:{scope}")
                    } else {
                        scope
                    },
                    body: Some(String::new()),
                    section: if procedure { "descriptor" } else { "body" }.into(),
                });
                bodies.push(SpoolBodyRef {
                    batch_index: 0,
                    document_index,
                    body,
                });
            }
        }
        if self.reader.position().map_err(map_error)? != *position {
            return Err(Error::SourceUnavailable(
                "Source changed during publication acquisition.".into(),
            ));
        }
        Ok((documents, bodies))
    }
    pub fn bootstrap_page(
        &self,
        worker: &agentlaw_worker::process::Client,
        basis: &SourcePosition,
        after: Option<&str>,
    ) -> Result<(
        agentlaw_worker::spool::PublishedSpoolPage,
        Option<String>,
        bool,
    )> {
        let (position, items, more) = self.reader.inventory_page(after, 64).map_err(map_error)?;
        if position.epoch != basis.epoch {
            return Err(Error::CoverageLost(
                "Source epoch changed during inventory.".into(),
            ));
        }
        let next = items.last().map(|(kind, id)| format!("{kind}/{id}"));
        let paths: Vec<_> = items
            .iter()
            .map(|(kind, id)| format!("current/{kind}/{}/{id}.md", &id[..2]))
            .collect();
        let (documents, bodies) = self.stage_paths(worker, &paths, &position)?;
        Ok((
            agentlaw_worker::spool::PublishedSpoolPage {
                page: PublishedPage {
                    basis: basis.clone(),
                    covered_through: basis.clone(),
                    source_position: basis.clone(),
                    batches: vec![PublishedBatch {
                        sequence: basis.sequence,
                        documents,
                    }],
                    coverage_complete: true,
                },
                bodies,
            },
            next,
            !more,
        ))
    }
    pub fn spooled_page(
        &self,
        worker: &agentlaw_worker::process::Client,
        basis: &SourcePosition,
    ) -> Result<agentlaw_worker::spool::PublishedSpoolPage> {
        use agentlaw_worker::spool::PublishedSpoolPage;
        let paths = self
            .reader
            .read_published_paths(
                &agentlaw_storage::published::SourcePosition {
                    epoch: basis.epoch.clone(),
                    sequence: basis.sequence,
                },
                1,
            )
            .map_err(map_error)?;
        let (documents, bodies) =
            self.stage_paths(worker, &paths.canonical_paths, &paths.source_position)?;
        let covered = SourcePosition {
            epoch: paths.covered_through.epoch,
            sequence: paths.covered_through.sequence,
        };
        let batches = if covered.sequence > basis.sequence {
            vec![PublishedBatch {
                sequence: covered.sequence,
                documents,
            }]
        } else {
            vec![]
        };
        Ok(PublishedSpoolPage {
            page: PublishedPage {
                basis: basis.clone(),
                covered_through: covered,
                source_position: SourcePosition {
                    epoch: paths.source_position.epoch,
                    sequence: paths.source_position.sequence,
                },
                batches,
                coverage_complete: true,
            },
            bodies,
        })
    }
}
fn map_error(error: agentlaw_storage::Error) -> Error {
    match error {
        agentlaw_storage::Error::CoverageLost => Error::CoverageLost(
            "Canonical publication ledger coverage is unavailable; rebuild is required.".into(),
        ),
        _ => Error::SourceUnavailable(
            "Canonical source reader failed; no empty coverage was substituted.".into(),
        ),
    }
}
impl<R: PublishedChangeSource> PublishedSourcePort for PublishedAdapter<R> {
    fn read_published_changes(
        &self,
        repository_id: &str,
        basis: &SourcePosition,
        limit: u32,
    ) -> Result<PublishedPage> {
        if repository_id != self.binding {
            return Err(Error::SourceUnavailable(
                "Derived source binding mismatch.".into(),
            ));
        }
        let page = self
            .reader
            .read_published_changes(
                &agentlaw_storage::published::SourcePosition {
                    epoch: basis.epoch.clone(),
                    sequence: basis.sequence,
                },
                limit,
            )
            .map_err(map_error)?;
        let mut batches = Vec::new();
        for batch in page.changes {
            let mut documents = Vec::new();
            for unit in batch.current_units {
                let procedure = unit.entity_type == "learned_procedure";
                match &unit.state {
                    UnitState::Live { heads } => {
                        for head in heads {
                            let applicability =
                                crate::runtime::scope_value(&head.metadata["applicability"])
                                    .map_err(|_| {
                                        Error::SourceUnavailable(
                                            "Invalid current scope in published source.".into(),
                                        )
                                    })?;
                            documents.push(DerivedDocument {
                                memory_id: if procedure {
                                    format!("procedure/{}", unit.entity_id)
                                } else {
                                    unit.entity_id.clone()
                                },
                                change_id: agentlaw_storage::version(&unit, head)
                                    .map_err(map_error)?,
                                scope: if procedure {
                                    format!(
                                        "procedure:{}",
                                        crate::runtime::scope_token(&applicability)
                                    )
                                } else {
                                    crate::runtime::scope_token(&applicability)
                                },
                                body: Some(if procedure {
                                    format!(
                                        "{}\n{}",
                                        head.metadata["name"].as_str().ok_or_else(|| {
                                            Error::SourceUnavailable(
                                                "Missing procedure name".into(),
                                            )
                                        })?,
                                        head.metadata["use_when"].as_str().ok_or_else(|| {
                                            Error::SourceUnavailable(
                                                "Missing procedure use_when".into(),
                                            )
                                        })?
                                    )
                                } else {
                                    head.body.clone()
                                }),
                                section: if procedure { "descriptor" } else { "body" }.into(),
                            })
                        }
                    }
                    UnitState::Redirect {
                        consolidation_change_id,
                        ..
                    } => documents.push(DerivedDocument {
                        memory_id: unit.entity_id.clone(),
                        change_id: consolidation_change_id.clone(),
                        scope: String::new(),
                        body: None,
                        section: "redirect".into(),
                    }),
                }
            }
            batches.push(PublishedBatch {
                sequence: batch.sequence,
                documents,
            });
        }
        Ok(PublishedPage {
            basis: basis.clone(),
            covered_through: SourcePosition {
                epoch: page.covered_through.epoch,
                sequence: page.covered_through.sequence,
            },
            source_position: SourcePosition {
                epoch: page.source_position.epoch,
                sequence: page.source_position.sequence,
            },
            batches,
            coverage_complete: true,
        })
    }
}
