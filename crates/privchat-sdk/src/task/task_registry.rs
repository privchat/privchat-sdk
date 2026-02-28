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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;

use super::task_handle::TaskHandle;

#[derive(Clone, Default)]
pub struct TaskRegistry {
    inner: Arc<TaskRegistryInner>,
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn track(&self, handle: JoinHandle<()>) -> TaskHandle {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .handles
            .lock()
            .expect("task registry poisoned")
            .insert(id, handle);
        TaskHandle::new(id, Arc::downgrade(&self.inner))
    }

    #[allow(dead_code)]
    pub fn cancel(&self, id: u64) -> bool {
        self.inner.cancel(id)
    }

    pub async fn shutdown(&self) {
        let handles = {
            let mut locked = self.inner.handles.lock().expect("task registry poisoned");
            locked.drain().map(|(_, handle)| handle).collect::<Vec<_>>()
        };
        for handle in handles {
            handle.abort();
            let _ = handle.await;
        }
    }
}

pub(crate) struct TaskRegistryInner {
    pub(crate) next_id: AtomicU64,
    pub(crate) handles: Mutex<HashMap<u64, JoinHandle<()>>>,
}

impl Default for TaskRegistryInner {
    fn default() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            handles: Mutex::new(HashMap::new()),
        }
    }
}

impl TaskRegistryInner {
    #[allow(dead_code)]
    pub(crate) fn cancel(&self, id: u64) -> bool {
        let handle = self
            .handles
            .lock()
            .expect("task registry poisoned")
            .remove(&id);
        if let Some(handle) = handle {
            handle.abort();
            true
        } else {
            false
        }
    }
}
