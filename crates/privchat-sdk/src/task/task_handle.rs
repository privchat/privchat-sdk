// Copyright 2025 Shanghai Boyu Information Technology Co., Ltd.
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

use std::sync::Weak;

use super::task_registry::TaskRegistryInner;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct TaskHandle {
    id: u64,
    registry: Weak<TaskRegistryInner>,
}

impl TaskHandle {
    pub(crate) fn new(id: u64, registry: Weak<TaskRegistryInner>) -> Self {
        Self { id, registry }
    }

    #[allow(dead_code)]
    pub fn id(&self) -> u64 {
        self.id
    }

    #[allow(dead_code)]
    pub fn cancel(&self) -> bool {
        let Some(registry) = self.registry.upgrade() else {
            return false;
        };
        registry.cancel(self.id)
    }
}
