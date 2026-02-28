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

use privchat_sdk_ffi::{PrivchatClient, PrivchatConfig, ServerEndpoint, TransportProtocol};
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let username = std::env::var("PRIVCHAT_USERNAME").unwrap_or_else(|_| "demo".to_string());
    let password = std::env::var("PRIVCHAT_PASSWORD").unwrap_or_else(|_| "123456".to_string());
    let device_id = std::env::var("PRIVCHAT_DEVICE_ID")
        .unwrap_or_else(|_| "00000000-0000-4000-8000-000000000001".to_string());

    let host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("PRIVCHAT_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(9001);
    let data_dir = std::env::var("PRIVCHAT_DATA_DIR").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("privchat-rust-basic")
            .to_string_lossy()
            .to_string()
    });

    let config = PrivchatConfig {
        endpoints: vec![
            ServerEndpoint {
                protocol: TransportProtocol::Quic,
                host: host.clone(),
                port,
                path: None,
                use_tls: false,
            },
            ServerEndpoint {
                protocol: TransportProtocol::Tcp,
                host: host.clone(),
                port,
                path: None,
                use_tls: false,
            },
            ServerEndpoint {
                protocol: TransportProtocol::WebSocket,
                host,
                port: 9080,
                path: Some("/".to_string()),
                use_tls: false,
            },
        ],
        connection_timeout_secs: 15,
        data_dir,
    };

    eprintln!("[basic] create");
    let client = match PrivchatClient::new(config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[basic] create failed: {e}");
            std::process::exit(1);
        }
    };

    eprintln!("[basic] connect");
    if let Err(e) = client.connect().await {
        eprintln!("[basic] connect failed: {e}");
        std::process::exit(2);
    }
    let state_after_connect = match client.connection_state().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] connection_state(after connect) failed: {e}");
            std::process::exit(30);
        }
    };
    if !matches!(
        state_after_connect,
        privchat_sdk_ffi::ConnectionState::Connected
    ) {
        eprintln!("[basic] expected Connected after connect, got {state_after_connect:?}");
        std::process::exit(31);
    }

    eprintln!("[basic] login");
    let login = match client.login(username, password, device_id).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] login failed: {e}");
            std::process::exit(3);
        }
    };

    eprintln!("[basic] authenticate");
    if let Err(e) = client
        .authenticate(login.user_id, login.token.clone(), login.device_id.clone())
        .await
    {
        eprintln!("[basic] authenticate failed: {e}");
        std::process::exit(4);
    }
    let state_after_auth = match client.connection_state().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] connection_state(after auth) failed: {e}");
            std::process::exit(32);
        }
    };
    if !matches!(
        state_after_auth,
        privchat_sdk_ffi::ConnectionState::Authenticated
    ) {
        eprintln!("[basic] expected Authenticated after authenticate, got {state_after_auth:?}");
        std::process::exit(33);
    }

    eprintln!("[basic] bootstrap");
    if let Err(e) = client.run_bootstrap_sync().await {
        eprintln!("[basic] bootstrap failed: {e}");
        std::process::exit(6);
    }

    eprintln!("[basic] channel local smoke");
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if let Err(e) = client
        .upsert_channel(privchat_sdk_ffi::UpsertChannelInput {
            channel_id: 1,
            channel_type: 1,
            channel_name: "demo-channel".to_string(),
            channel_remark: "demo-remark".to_string(),
            avatar: "".to_string(),
            unread_count: 0,
            top: 0,
            mute: 0,
            last_msg_timestamp: now_ms,
            last_local_message_id: 0,
            last_msg_content: "".to_string(),
        })
        .await
    {
        eprintln!("[basic] upsert_channel failed: {e}");
        std::process::exit(34);
    }
    let ch = match client.get_channel_by_id(1).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] get_channel_by_id failed: {e}");
            std::process::exit(35);
        }
    };
    if ch.is_none() {
        eprintln!("[basic] expected channel 1 exists");
        std::process::exit(36);
    }
    let chs = match client.list_channels(20, 0).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] list_channels failed: {e}");
            std::process::exit(37);
        }
    };
    if chs.is_empty() {
        eprintln!("[basic] expected non-empty channels");
        std::process::exit(38);
    }
    if let Err(e) = client
        .upsert_channel_extra(privchat_sdk_ffi::UpsertChannelExtraInput {
            channel_id: 1,
            channel_type: 1,
            browse_to: 100,
            keep_pts: 200,
            keep_offset_y: 10,
            draft: "draft text".to_string(),
            draft_updated_at: now_ms as u64,
        })
        .await
    {
        eprintln!("[basic] upsert_channel_extra failed: {e}");
        std::process::exit(39);
    }
    let extra = match client.get_channel_extra(1, 1).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] get_channel_extra failed: {e}");
            std::process::exit(40);
        }
    };
    match extra {
        Some(v) => {
            if v.keep_pts != 200 || v.browse_to != 100 {
                eprintln!(
                    "[basic] unexpected channel_extra values: keep_pts={}, browse_to={}",
                    v.keep_pts, v.browse_to
                );
                std::process::exit(41);
            }
        }
        None => {
            eprintln!("[basic] expected channel_extra exists");
            std::process::exit(42);
        }
    }

    eprintln!("[basic] queue smoke");
    let message_id = match client
        .create_local_message(privchat_sdk_ffi::NewMessage {
            channel_id: 1,
            channel_type: 1,
            from_uid: login.user_id,
            message_type: 1,
            content: "hello".to_string(),
            searchable_word: "hello".to_string(),
            setting: 0,
            extra: "{}".to_string(),
        })
        .await
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] create message failed: {e}");
            std::process::exit(7);
        }
    };
    let page = match client.list_messages(1, 1, 20, 0).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] list_messages failed: {e}");
            std::process::exit(18);
        }
    };
    if !page.iter().any(|m| m.message_id == message_id) {
        eprintln!("[basic] list_messages missing message_id={message_id}");
        std::process::exit(19);
    }
    if let Err(e) = client.update_message_status(message_id, 1).await {
        eprintln!("[basic] update_message_status failed: {e}");
        std::process::exit(12);
    }
    if let Err(e) = client.set_message_read(message_id, 1, 1, false).await {
        eprintln!("[basic] set_message_read(false) failed: {e}");
        std::process::exit(24);
    }
    let unread = match client.get_channel_unread_count(1, 1).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] get_channel_unread_count failed: {e}");
            std::process::exit(25);
        }
    };
    if unread <= 0 {
        eprintln!("[basic] expected unread > 0 after set_message_read(false), got {unread}");
        std::process::exit(26);
    }
    if let Err(e) = client.mark_channel_read(1, 1).await {
        eprintln!("[basic] mark_channel_read failed: {e}");
        std::process::exit(27);
    }
    let unread_after = match client.get_channel_unread_count(1, 1).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] get_channel_unread_count(after) failed: {e}");
            std::process::exit(28);
        }
    };
    if unread_after != 0 {
        eprintln!("[basic] expected unread_after=0, got {unread_after}");
        std::process::exit(29);
    }
    if let Err(e) = client
        .kv_put(
            "basic:last_message_id".to_string(),
            message_id.to_string().into_bytes(),
        )
        .await
    {
        eprintln!("[basic] kv_put failed: {e}");
        std::process::exit(13);
    }
    let kv_v = match client.kv_get("basic:last_message_id".to_string()).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] kv_get failed: {e}");
            std::process::exit(14);
        }
    };
    if kv_v.is_none() {
        eprintln!("[basic] kv_get empty");
        std::process::exit(15);
    }
    let paths = match client.user_storage_paths().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] user_storage_paths failed: {e}");
            std::process::exit(16);
        }
    };
    if paths.db_path.is_empty() || paths.queue_root.is_empty() {
        eprintln!("[basic] invalid storage paths");
        std::process::exit(17);
    }
    let message_id = match client
        .enqueue_outbound_message(
            message_id,
            b"{\"kind\":\"text\",\"body\":\"hello\"}".to_vec(),
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] enqueue failed: {e}");
            std::process::exit(8);
        }
    };
    let items = match client.peek_outbound_messages(10).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] peek failed: {e}");
            std::process::exit(9);
        }
    };
    if !items.iter().any(|m| m.message_id == message_id) {
        eprintln!("[basic] queue missing message_id={message_id}");
        std::process::exit(10);
    }
    if let Err(e) = client.ack_outbound_messages(vec![message_id]).await {
        eprintln!("[basic] ack failed: {e}");
        std::process::exit(11);
    }

    eprintln!("[basic] ok user_id={}", login.user_id);
    let connected = match client.is_connected().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[basic] is_connected failed: {e}");
            std::process::exit(20);
        }
    };
    if !connected {
        eprintln!("[basic] expected connected=true");
        std::process::exit(21);
    }
    if let Err(e) = client.ping().await {
        eprintln!("[basic] ping failed: {e}");
        std::process::exit(22);
    }
    if let Err(e) = client.disconnect().await {
        eprintln!("[basic] disconnect failed: {e}");
        std::process::exit(23);
    }
    eprintln!("[basic] shutdown");
    if let Err(e) = client.shutdown().await {
        eprintln!("[basic] shutdown failed: {e}");
        std::process::exit(5);
    }
}
