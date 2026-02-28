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

pub mod domain {
    pub const TRANSPORT: u32 = 0x01;
    pub const STORAGE: u32 = 0x02;
    pub const AUTH: u32 = 0x03;
    pub const SYNC: u32 = 0x04;
    pub const STATE: u32 = 0x05;
    pub const ACTOR: u32 = 0x06;
    pub const SERIALIZATION: u32 = 0x07;
    pub const SHUTDOWN: u32 = 0x08;
    pub const INTERNAL: u32 = 0x0F;
}

const fn code(domain: u32, detail: u32) -> u32 {
    (domain << 24) | (detail & 0x00FF_FFFF)
}

pub const TRANSPORT_FAILURE: u32 = code(domain::TRANSPORT, 1);
pub const NETWORK_DISCONNECTED: u32 = code(domain::TRANSPORT, 2);
pub const AUTH_FAILURE: u32 = code(domain::AUTH, 1);
pub const STORAGE_FAILURE: u32 = code(domain::STORAGE, 1);
pub const SERIALIZATION_FAILURE: u32 = code(domain::SERIALIZATION, 1);
pub const INVALID_STATE: u32 = code(domain::STATE, 1);
pub const ACTOR_CLOSED: u32 = code(domain::ACTOR, 1);
pub const SHUTDOWN: u32 = code(domain::SHUTDOWN, 1);
pub const INTERNAL_UNKNOWN: u32 = code(domain::INTERNAL, 1);
