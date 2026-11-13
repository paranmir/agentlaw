//! Store access for Git work without exposing the private store.

use crate::store::lane::{
    ImportPublishInput, LaneCommand, LaneReply, LaneSender, SyncOverlayInput,
};
use agentlaw_flows::{DomainError, RequestControl, Result};
use agentlaw_storage::{published::OwnedPublishedReader, PublishReceipt};
use std::path::PathBuf;

pub struct GitStoreHandle {
    pub(crate) root: PathBuf,
    pub(crate) local: PathBuf,
    pub(crate) coordination: PathBuf,
    pub(crate) source: OwnedPublishedReader,
    pub(crate) lane: LaneSender,
}

impl GitStoreHandle {
    pub fn sync_control(&self) -> super::SyncControl {
        super::SyncControl {
            local: self.local.clone(),
            lane: self.lane.clone(),
        }
    }
    pub fn publish_sync_overlay(
        &self,
        input: SyncOverlayInput,
        control: &RequestControl,
    ) -> Result<PublishReceipt> {
        match self
            .lane
            .call(LaneCommand::PublishSyncOverlay(input), control)?
        {
            LaneReply::Published(receipt) => Ok(receipt),
            _ => Err(DomainError::new(
                "store_unavailable",
                "Unexpected publication lane reply.",
            )),
        }
    }
    pub fn publish_import(
        &self,
        input: ImportPublishInput,
        control: &RequestControl,
    ) -> Result<PublishReceipt> {
        match self.lane.call(LaneCommand::PublishImport(input), control)? {
            LaneReply::Published(receipt) => Ok(receipt),
            _ => Err(DomainError::new(
                "store_unavailable",
                "Unexpected publication lane reply.",
            )),
        }
    }
}
#[cfg(test)]
mod tests;
