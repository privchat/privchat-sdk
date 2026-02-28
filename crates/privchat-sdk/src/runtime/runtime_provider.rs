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

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::{Builder, Handle, Runtime};
use tokio::task::JoinHandle;

#[derive(Clone)]
pub struct RuntimeProvider {
    handle: Handle,
}

static SHARED_RUNTIME: OnceLock<Runtime> = OnceLock::new();

impl RuntimeProvider {
    pub fn from_handle(handle: Handle) -> Self {
        Self { handle }
    }

    pub fn new_owned() -> Self {
        let runtime = SHARED_RUNTIME.get_or_init(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("privchat-sdk runtime")
        });
        let handle = runtime.handle().clone();
        Self { handle }
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    pub fn spawn<F>(&self, fut: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle.spawn(fut)
    }
}
