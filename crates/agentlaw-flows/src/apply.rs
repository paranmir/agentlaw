//! Publishing retained work and recording its results.
use crate::runtime::*;
use crate::*;
use agentlaw_storage::UnitState;
use rusqlite::params;
use serde_json::{json, Value};
impl Runtime {
    pub(crate) fn publish_retained(&self, mut retained: Retained) -> Result<Value> {
        let generation = retained.expected_generation.ok_or_else(|| {
            DomainError::new(
                "control_corrupt",
                "Retained execution lacks its source generation.",
            )
        })?;
        self.request_control.phase("publishing");
        let publication = if let Some(basis) = &retained.publication_basis {
            self.store.publish_with_readset(
                retained.operation_id.as_ref().unwrap(),
                retained.mutations.clone(),
                basis,
                Some(&self.request_control.cancel),
            )
        } else {
            self.store.publish_if_generation_control(
                retained.operation_id.as_ref().unwrap(),
                retained.mutations.clone(),
                generation,
                &self.request_control.cancel,
            )
        };
        match publication {
            Ok(receipt) => {
                self.request_control.phase("published");
                self.complete_pending(&retained, receipt)
            }
            Err(agentlaw_storage::Error::CancelledBeforeDecision) => {
                let old = retained.batch.reference.observed_version.clone();
                retained.operation_id = None;
                retained.expected_generation = None;
                retained.resolution_ref = Some(uuid::Uuid::new_v4().to_string());
                retained.mutations.clear();
                retained.batch.reference.observed_version = uuid::Uuid::new_v4().to_string();
                retained.issues = vec![
                    json!({"code":"cancelled","message":"Publication was cancelled before its durable decision. The proposal remains retained and will not publish automatically."}),
                ];
                self.save_pending(&retained, Some(&old))?;
                Ok(Self::pending_response(&retained, "pending"))
            }
            Err(agentlaw_storage::Error::Stale(_)) => {
                let old = retained.batch.reference.observed_version.clone();
                retained.operation_id = None;
                retained.expected_generation = None;
                retained.resolution_ref = Some(uuid::Uuid::new_v4().to_string());
                retained.mutations.clear();
                retained.basis.clear();
                retained.batch.reference.observed_version = uuid::Uuid::new_v4().to_string();
                retained.issues = vec![
                    json!({"code":"source_changed","message":"The source changed after review. Inspect retained proposals and continue to revalidate all heads and new dependencies."}),
                ];
                self.save_pending(&retained, Some(&old))?;
                Ok(Self::pending_response(&retained, "resolution_required"))
            }
            Err(e) => source(Err(e)),
        }
    }
    pub(crate) fn save_pending(&self, r: &Retained, old: Option<&str>) -> Result<()> {
        let payload = encode(r)?;
        if let Some(old) = old {
            let n=sql(self.control.execute("UPDATE pending SET version=?1,payload=?2 WHERE id=?3 AND version=?4 AND status='pending'",params![r.batch.reference.observed_version,payload,r.batch.reference.pending_batch_id,old]))?;
            if n != 1 {
                return Err(DomainError::new(
                    "stale_pending_ref",
                    "Pending proposals changed concurrently; inspect latest retained work.",
                ));
            }
        } else {
            sql(self.control.execute("INSERT INTO pending(id,version,binding,payload,status) VALUES(?1,?2,?3,?4,'pending')",params![r.batch.reference.pending_batch_id,r.batch.reference.observed_version,r.batch.store_binding_id,payload]))?;
        }
        Ok(())
    }
    pub(crate) fn complete_pending(
        &self,
        r: &Retained,
        receipt: agentlaw_storage::PublishReceipt,
    ) -> Result<Value> {
        let results=r.mutations.iter().filter(|m|matches!(m.unit.state,UnitState::Live{..})).enumerate().map(|(index,m)|{
            let reference=receipt.references.iter().find(|reference|reference.memory_id==m.unit.entity_id).ok_or_else(||DomainError::new("publication_receipt_incomplete","Published receipt lacks a proposal destination; preserve retained execution and inspect source."))?;
            Ok(json!({"proposal_index":index,"memory_ref":{"memory_id":reference.memory_id,"observed_version":reference.observed_version}}))
        }).collect::<Result<Vec<_>>>()?;
        let result = json!({"status":"remembered","results":results,"diagnostics":r.diagnostics});
        // The background reader consumes the durable source ledger. Publication
        // completion does not wait for historical indexing or embedding work.
        sql(self.control.execute(
            "UPDATE pending SET status='published',result=?1 WHERE id=?2",
            params![encode(&result)?, r.batch.reference.pending_batch_id],
        ))?;
        Ok(result)
    }
    /// Schedule durable indexing through the authenticated worker IPC. Source
    /// access remains in this read-only adapter; the worker receives no paths or
    /// canonical write authority. Queue acceptance is not an index acknowledgment.
    pub fn enqueue_derived(&self) -> Result<()> {
        let worker = self.worker.as_ref().ok_or_else(|| {
            DomainError::new(
                "indexing_pending",
                "No worker is attached; source publication remains complete.",
            )
        })?;
        crate::read::pump(
            worker,
            &crate::derived::PublishedAdapter::new(
                self.store.owned_published_reader(),
                self.binding_id.clone(),
            ),
            &self.binding_id,
            &self.local,
            &self.request_control.cancel,
        )
    }
}
