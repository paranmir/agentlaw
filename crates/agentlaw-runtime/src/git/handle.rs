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
    pub(crate) fn with_source_read<T>(
        &self,
        read: impl FnOnce(&std::path::Path, u64) -> agentlaw_storage::Result<T>,
    ) -> agentlaw_storage::Result<T> {
        self.source.with_source_read(read)
    }
    pub(crate) fn coordination_root(&self) -> &std::path::Path {
        &self.coordination
    }
    pub(crate) fn generation(&self) -> agentlaw_storage::Result<u64> {
        self.source.with_source_read(|_, generation| Ok(generation))
    }
    pub(crate) fn validate_imported_source(
        &self,
        staged: &agentlaw_storage::Store,
        generation: u64,
    ) -> agentlaw_storage::Result<()> {
        self.source.validate_imported_source(staged, generation)
    }
    pub(crate) fn prepare_import_union(
        &self,
        incoming: &agentlaw_storage::Store,
        root: &std::path::Path,
        local: &std::path::Path,
    ) -> agentlaw_storage::Result<agentlaw_storage::Store> {
        self.source.prepare_import_union(incoming, root, local)
    }
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
    pub(crate) fn import_receipt(
        &self,
        operation_id: &str,
        control: &RequestControl,
    ) -> Result<Option<PublishReceipt>> {
        match self
            .lane
            .call(LaneCommand::ImportReceipt(operation_id.to_owned()), control)?
        {
            LaneReply::ImportReceipt(receipt) => Ok(receipt),
            _ => Err(DomainError::new(
                "store_unavailable",
                "Unexpected import receipt lane reply.",
            )),
        }
    }
}
#[cfg(test)]
mod tests;
