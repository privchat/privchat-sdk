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

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct AccountConfig {
    pub key: String,
    pub username: String,
    pub password: String,
    pub user_id: u64,
    pub token: String,
    pub device_id: String,
    pub data_dir: String,
}

#[derive(Debug, Default, Clone)]
pub struct PhaseMetrics {
    pub rpc_calls: u32,
    pub rpc_successes: u32,
    pub messages_sent: u32,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PhaseResult {
    pub phase_name: String,
    pub success: bool,
    pub duration: Duration,
    pub details: String,
    pub metrics: PhaseMetrics,
}

#[derive(Debug, Default)]
pub struct TestSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub duration: Duration,
    pub results: Vec<PhaseResult>,
}
