// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::{HashSet, VecDeque};

use privchat_protocol::rpc::sync::SyncEntityItem;

#[derive(Debug, Clone)]
pub struct SyncBatch {
    pub entity_type: String,
    pub scope: Option<String>,
    pub items: Vec<SyncEntityItem>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ReceivePipelineStats {
    pub queued_items: usize,
    pub dropped_duplicates: usize,
}

pub struct ReceivePipeline {
    queue: VecDeque<SyncBatch>,
    recent_keys: HashSet<String>,
    recent_key_order: VecDeque<String>,
    recent_key_limit: usize,
}

impl Default for ReceivePipeline {
    fn default() -> Self {
        Self::new(8192)
    }
}

impl ReceivePipeline {
    pub fn new(recent_key_limit: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            recent_keys: HashSet::new(),
            recent_key_order: VecDeque::new(),
            recent_key_limit: recent_key_limit.max(512),
        }
    }

    pub fn enqueue(
        &mut self,
        entity_type: String,
        scope: Option<String>,
        items: Vec<SyncEntityItem>,
    ) -> ReceivePipelineStats {
        let mut accepted = Vec::with_capacity(items.len());
        let mut dropped_duplicates = 0usize;
        for item in items {
            let key = dedupe_key(&entity_type, scope.as_deref(), &item);
            if self.recent_keys.contains(&key) {
                dropped_duplicates += 1;
                continue;
            }
            self.remember_key(key);
            accepted.push(item);
        }
        let queued_items = accepted.len();
        if !accepted.is_empty() {
            self.queue.push_back(SyncBatch {
                entity_type,
                scope,
                items: accepted,
            });
        }
        ReceivePipelineStats {
            queued_items,
            dropped_duplicates,
        }
    }

    pub fn pop_batch(&mut self) -> Option<SyncBatch> {
        self.queue.pop_front()
    }

    pub fn requeue_front(&mut self, batch: SyncBatch) {
        self.queue.push_front(batch);
    }

    fn remember_key(&mut self, key: String) {
        self.recent_keys.insert(key.clone());
        self.recent_key_order.push_back(key);
        while self.recent_key_order.len() > self.recent_key_limit {
            if let Some(old) = self.recent_key_order.pop_front() {
                self.recent_keys.remove(&old);
            }
        }
    }
}

fn dedupe_key(entity_type: &str, scope: Option<&str>, item: &SyncEntityItem) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        entity_type,
        scope.unwrap_or_default(),
        item.entity_id,
        item.version,
        item.deleted
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(entity_id: &str, version: u64) -> SyncEntityItem {
        SyncEntityItem {
            entity_id: entity_id.to_string(),
            version,
            deleted: false,
            payload: Some(serde_json::json!({"id": entity_id, "v": version})),
        }
    }

    #[test]
    fn duplicate_push_is_deduped() {
        let mut pipeline = ReceivePipeline::new(1024);
        let stats_1 = pipeline.enqueue("message".to_string(), None, vec![item("1", 10)]);
        let stats_2 = pipeline.enqueue("message".to_string(), None, vec![item("1", 10)]);
        assert_eq!(stats_1.queued_items, 1);
        assert_eq!(stats_2.queued_items, 0);
        assert_eq!(stats_2.dropped_duplicates, 1);
    }

    #[test]
    fn out_of_order_versions_are_not_dropped_by_dedupe() {
        let mut pipeline = ReceivePipeline::new(1024);
        let stats_1 = pipeline.enqueue("message".to_string(), None, vec![item("1", 10)]);
        let stats_2 = pipeline.enqueue("message".to_string(), None, vec![item("1", 9)]);
        assert_eq!(stats_1.queued_items, 1);
        assert_eq!(stats_2.queued_items, 1);
        assert_eq!(stats_2.dropped_duplicates, 0);
    }
}
