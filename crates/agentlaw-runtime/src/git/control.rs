use crate::store::lane::{LaneCommand, LaneReply, LaneSender, SyncOperationRow};
use agentlaw_flows::{control::ControlReader, DomainError, RequestControl, Result};
use std::path::{Path, PathBuf};

pub struct SyncControl {
    pub(super) local: PathBuf,
    pub(super) lane: LaneSender,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SyncReceipt {
    pub operation_id: String,
    pub digest: String,
    pub response: Option<String>,
}
impl SyncControl {
    pub fn any_active(&self) -> Result<bool> {
        ControlReader::open(&self.local)?.sync_any_active()
    }
    pub fn active_for(&self, source: &Path) -> Result<Option<String>> {
        ControlReader::open(&self.local)?.sync_active_for(&source.to_string_lossy())
    }
    pub(crate) fn request(&self, id: &str) -> Result<Option<SyncReceipt>> {
        Ok(ControlReader::open(&self.local)?.sync_request(id)?.map(
            |(operation_id, digest, response)| SyncReceipt {
                operation_id,
                digest,
                response,
            },
        ))
    }
    pub fn operation(&self, id: &str) -> Result<Option<String>> {
        ControlReader::open(&self.local)?.sync_operation(id)
    }
    pub fn persist(&self, row: SyncOperationRow, control: &RequestControl) -> Result<()> {
        match self.lane.call(LaneCommand::SyncPersist(row), control)? {
            LaneReply::Done => Ok(()),
            _ => Err(DomainError::new(
                "store_unavailable",
                "Unexpected sync lane reply.",
            )),
        }
    }
    pub fn reserve(
        &self,
        id: &str,
        operation: &str,
        digest: &str,
        control: &RequestControl,
    ) -> Result<bool> {
        match self.lane.call(
            LaneCommand::SyncReserve {
                id: id.into(),
                operation: operation.into(),
                digest: digest.into(),
            },
            control,
        )? {
            LaneReply::Reserved(value) => Ok(value),
            _ => Err(DomainError::new(
                "store_unavailable",
                "Unexpected sync lane reply.",
            )),
        }
    }
    pub fn complete(
        &self,
        id: &str,
        response_json: String,
        control: &RequestControl,
    ) -> Result<()> {
        match self.lane.call(
            LaneCommand::SyncComplete {
                id: id.into(),
                response_json,
            },
            control,
        )? {
            LaneReply::Done => Ok(()),
            _ => Err(DomainError::new(
                "store_unavailable",
                "Unexpected sync lane reply.",
            )),
        }
    }
}
#[cfg(test)]
mod tests;
