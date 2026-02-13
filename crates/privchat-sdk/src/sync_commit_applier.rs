use crate::receive_pipeline::{ReceivePipeline, SyncBatch};

pub struct SyncCommitApplier;

impl SyncCommitApplier {
    pub fn drain_batches(pipeline: &mut ReceivePipeline) -> Vec<SyncBatch> {
        let mut batches = Vec::new();
        while let Some(batch) = pipeline.pop_batch() {
            batches.push(batch);
        }
        batches
    }

    pub fn requeue_front(pipeline: &mut ReceivePipeline, batch: SyncBatch) {
        pipeline.requeue_front(batch);
    }
}
