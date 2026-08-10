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

//! 附件秒传的真链路验证：真 SDK、真连接、真服务端、真对象存储。
//!
//! 验的是两件事，也只有这两件：
//!
//! 1. **没上传过的内容 → miss，老流程照走。** 顺带覆盖「不带摘要」的老客户端，
//!    它们必须还能上传（`already_exists` 缺省当 false）。
//! 2. **已经在服务端的内容 → hit，换到自己的 `file_id`，正文零字节。**
//!    两条记录指向同一个物理文件——这就是「转发不重新上传」的全部含义。
//!
//! 🔴 这里刻意**不**验「同一张图片封两次会命中」，因为按设计它不会：每次封装用
//! 新的随机 CEK/nonce，密文不同 ⇒ 摘要不同 ⇒ 两个物理文件。那是正确行为，不是缺陷。
//! 命中只发生在客户端手里就是服务端已有的那串字节时。
//!
//! 跑法（需要本机 server 起着）：
//! ```bash
//! cargo run -p privchat-sdk --example dedup
//! ```

use std::time::{SystemTime, UNIX_EPOCH};

use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, TransportProtocol};
use serde_json::{json, Value};
use sha2::Digest as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

#[tokio::main]
async fn main() -> BoxResult<()> {
    let host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let tcp_port: u16 = std::env::var("PRIVCHAT_TCP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9001);

    let ts = now_millis();
    let data_dir =
        std::env::temp_dir().join(format!("privchat-dedup-{}-{}", ts, std::process::id()));
    std::fs::create_dir_all(&data_dir)?;

    let sdk = PrivchatSdk::new(PrivchatConfig {
        endpoints: vec![ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host,
            port: tcp_port,
            path: None,
            use_tls: false,
        }],
        connection_timeout_secs: 30,
        data_dir: data_dir.to_string_lossy().to_string(),
    });

    println!("1) connect + register + authenticate");
    sdk.connect().await?;
    let username = format!("dedup_{}{}", ts % 100000, std::process::id());
    let login = sdk
        .register(username.clone(), "password123".to_string(), uuid_like())
        .await?;
    sdk.authenticate(login.user_id, login.token.clone(), login.device_id.clone())
        .await?;
    println!("   user_id={}", login.user_id);

    // ---------------------------------------------------------------- 场景 1
    println!("\n2) 全新内容 → 必须 miss，走完整上传");
    let blob_a = random_blob(ts);
    let digest_a = sha256_hex(&blob_a);
    let token_a = request_token(&sdk, blob_a.len(), Some(&digest_a)).await?;
    assert_eq!(
        token_a["already_exists"].as_bool(),
        Some(false),
        "🔴 服务端从没见过这串字节，预检必须是 miss"
    );
    let file_a = upload_blob(&token_a, &blob_a).await?;
    let (id_a, url_a) = (file_id(&file_a)?, file_url(&file_a)?);
    println!("   file_id={} url={}", id_a, url_a);

    // ---------------------------------------------------------------- 场景 2
    println!("\n3) 同一串字节再预检 → 必须 hit（服务端自己算出的摘要对得上）");
    let token_probe = request_token(&sdk, blob_a.len(), Some(&digest_a)).await?;
    assert_eq!(
        token_probe["already_exists"].as_bool(),
        Some(true),
        "🔴 内容已经在服务端，预检必须命中"
    );
    assert!(
        token_probe["file_id"].as_str().unwrap_or_default().is_empty(),
        "🔴 预检不许泄露别人的 file_id，它只回答「在不在」；实际={}",
        token_probe["file_id"]
    );

    // ---------------------------------------------------------------- 场景 3
    println!("\n4) get_url 必须下发真实 file_type（客户端不许靠 mime 猜）");
    let detail = get_url(&sdk, id_a.parse::<u64>()?).await?;
    assert_eq!(
        detail["file_type"].as_str(),
        Some("file"),
        "🔴 服务端要下发它自己记的类型；空串会把客户端逼回 mime 推导那张兜底表"
    );

    // ---------------------------------------------------------------- 场景 4
    println!("\n5) 秒传取用：换自己的 file_id，正文零字节");
    let claimed = claim_existing(&sdk, &token_probe, &digest_a).await?;
    let (claimed_id, claimed_url) = (file_id(&claimed)?, file_url(&claimed)?);
    println!("   file_id={} url={}", claimed_id, claimed_url);
    assert_ne!(
        claimed_id, id_a,
        "🔴 必须是**自己的**一条新记录，不是把源记录递回来"
    );
    assert_eq!(
        claimed_url, url_a,
        "🔴 两条记录必须指向同一个物理文件——这就是「不重新上传」"
    );

    // ---------------------------------------------------------------- 场景 4
    println!("\n6) 不带摘要的老客户端 → 照常上传，不受影响");
    let blob_b = random_blob(ts.wrapping_add(7919));
    let token_b = request_token(&sdk, blob_b.len(), None).await?;
    assert_ne!(
        token_b["already_exists"].as_bool(),
        Some(true),
        "🔴 没申报摘要就没法判重，不许假装命中"
    );
    let file_b = upload_blob(&token_b, &blob_b).await?;
    let url_b = file_url(&file_b)?;
    assert_ne!(url_b, url_a, "🔴 不同内容必须是不同的物理文件");
    println!("   file_id={} url={}", file_id(&file_b)?, url_b);

    // ---------------------------------------------------------------- 场景 5
    println!("\n7) 重新封装同一份明文 → 密文变了，必须 miss（预期行为，不是缺陷）");
    let resealed = random_blob(ts.wrapping_add(31337)); // 等价于换了随机 CEK/nonce
    let token_c = request_token(&sdk, resealed.len(), Some(&sha256_hex(&resealed))).await?;
    assert_eq!(
        token_c["already_exists"].as_bool(),
        Some(false),
        "🔴 字节不同就是另一个文件，不许命中"
    );

    sdk.disconnect().await?;
    println!("\n✅ 秒传真链路全部通过");
    Ok(())
}

// ---------------------------------------------------------------------------

async fn claim_existing(sdk: &PrivchatSdk, token: &Value, sha256: &str) -> BoxResult<Value> {
    let body = json!({
        "token": token["token"].as_str().ok_or("token response missing token")?,
        "sha256": sha256,
    });
    Ok(serde_json::from_str(
        &sdk.rpc_call("file/claim_existing".to_string(), body.to_string())
            .await?,
    )?)
}

async fn get_url(sdk: &PrivchatSdk, file_id: u64) -> BoxResult<Value> {
    Ok(serde_json::from_str(
        &sdk.rpc_call(
            "file/get_url".to_string(),
            json!({ "file_id": file_id, "user_id": 0 }).to_string(),
        )
        .await?,
    )?)
}

async fn request_token(sdk: &PrivchatSdk, size: usize, sha256: Option<&str>) -> BoxResult<Value> {
    let mut body = json!({
        "user_id": 0,
        "filename": "dedup-probe.bin",
        "file_size": size as i64,
        "mime_type": "application/octet-stream",
        "file_type": "file",
        "business_type": "message",
        "transform_version": 0,
    });
    if let Some(d) = sha256 {
        body["sha256"] = json!(d);
    }
    Ok(serde_json::from_str(
        &sdk.rpc_call("file/request_upload_token".to_string(), body.to_string())
            .await?,
    )?)
}

async fn upload_blob(token: &Value, blob: &[u8]) -> BoxResult<Value> {
    let upload_url = token["upload_url"]
        .as_str()
        .ok_or("token response missing upload_url")?;
    let upload_token = token["token"].as_str().ok_or("token response missing token")?;
    let part = reqwest::multipart::Part::bytes(blob.to_vec())
        .file_name("dedup-probe.bin")
        .mime_str("application/octet-stream")?;
    let form = reqwest::multipart::Form::new().part("file", part);
    let response = reqwest::Client::new()
        .post(upload_url)
        .header("X-Upload-Token", upload_token)
        .multipart(form)
        .send()
        .await?;
    let status = response.status();
    let envelope: Value = response.json().await?;
    if !status.is_success() {
        return Err(format!("upload failed: status={status} body={envelope}").into());
    }
    Ok(envelope)
}

fn payload(envelope: &Value) -> &Value {
    envelope.get("data").unwrap_or(envelope)
}

fn file_id(envelope: &Value) -> BoxResult<String> {
    let v = &payload(envelope)["file_id"];
    v.as_str()
        .map(str::to_string)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
        .ok_or_else(|| format!("missing file_id in {envelope}").into())
}

fn file_url(envelope: &Value) -> BoxResult<String> {
    payload(envelope)["file_url"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("missing file_url in {envelope}").into())
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// 造一串确定性的伪随机字节：够长到不会跟别的测试撞车，且同一个种子可复现。
fn random_blob(seed: u128) -> Vec<u8> {
    let mut state = seed as u64 | 1;
    (0..4096)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn uuid_like() -> String {
    let n = now_millis();
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (n >> 64) as u32,
        (n >> 48) as u16,
        (n >> 36) as u16 & 0xfff,
        (n >> 24) as u16 & 0xfff,
        n as u64 & 0xffff_ffff_ffff
    )
}
