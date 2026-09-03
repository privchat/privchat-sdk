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
//! 判重键是**明文摘要**。密文摘要做不了判重：每块都用新的随机 nonce，同一份
//! 明文封两次就是两串不同的字节，按密文永远不可能命中。所以这里验的是：
//!
//! 1. **没上传过的明文 → miss，照常上传密文。**
//! 1b. **不申报明文摘要 → 最终落库被拒**：没有冻结的身份就没有东西可复核，
//!    verify-before-publish 在这里必须 fail-closed。
//! 2. **同一份明文再探测 → hit**，换到自己的 `file_id`，正文零字节，两条记录
//!    指向同一个物理文件。
//! 3. **重新封装同一份明文 → 仍然 hit**。这一条是判重键从密文换成明文的全部
//!    意义所在；写反了就等于回到旧模型。
//! 4. **换一个毫不相干的用户 → 仍然 hit**，且探测不泄露别人的 `file_id`。
//!    跨用户秒传是这套设计要的结果，不是副作用。
//! 5. 不同明文 → 不同物理文件。
//!
//! 跑法（需要本机 server 起着）：
//! ```bash
//! export PRIVCHAT_SPKI_PINS=$(openssl x509 -in ../privchat-server/certs/server.crt \
//!   -pubkey -noout | openssl pkey -pubin -outform der \
//!   | openssl dgst -sha256 -binary | base64)
//! cargo run -p privchat-sdk --example dedup
//! ```

use std::time::{SystemTime, UNIX_EPOCH};

use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, TransportProtocol};
use serde_json::{json, Value};
use sha2::Digest as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

/// PLATFORM 模式的部署里 server 的内置注册是关掉的（account.mode=PLATFORM），
/// 账号由 platform 侧签发。所以这里允许直接喂一组已有凭据：
///   PRIVCHAT_UID_A / PRIVCHAT_TOKEN_A / PRIVCHAT_DEVICE_A（第二个用户用 _B）
/// 三个都给了就跳过注册，否则走 server 内置注册（BUILTIN 本地环境）。
async fn sign_in(
    sdk: &PrivchatSdk,
    suffix: &str,
    fallback_username: String,
) -> BoxResult<u64> {
    let uid = std::env::var(format!("PRIVCHAT_UID_{suffix}")).ok();
    let token = std::env::var(format!("PRIVCHAT_TOKEN_{suffix}")).ok();
    let device = std::env::var(format!("PRIVCHAT_DEVICE_{suffix}")).ok();
    if let (Some(uid), Some(token), Some(device)) = (uid, token, device) {
        let uid: u64 = uid.parse()?;
        sdk.authenticate(uid, token, device).await?;
        return Ok(uid);
    }
    let login = sdk
        .register(fallback_username, "password123".to_string(), uuid_like())
        .await?;
    sdk.authenticate(login.user_id, login.token.clone(), login.device_id.clone())
        .await?;
    Ok(login.user_id)
}

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

    let sdk = PrivchatSdk::new(sdk_config(&host, tcp_port, &data_dir));

    println!("1) connect + register + authenticate");
    sdk.connect().await?;
    let username = format!("dedup_{}{}", ts % 100000, std::process::id());
    let my_uid = sign_in(&sdk, "A", username.clone()).await?;
    println!("   user_id={}", my_uid);

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
    let file_a = upload_blob(&token_a, &seal_with_token(&token_a, &blob_a)?).await?;
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
    println!("\n6) 不申报明文摘要 → 判重不命中，且最终不许落库（fail-closed）");
    let blob_b = random_blob(ts.wrapping_add(7919));
    let token_b = request_token(&sdk, blob_b.len(), None).await?;
    assert_ne!(
        token_b["already_exists"].as_bool(),
        Some(true),
        "🔴 没申报摘要就没法判重，不许假装命中"
    );
    // 🔴 没有冻结的明文身份就没有东西可复核，服务端必须拒绝，而不是"先存下来再说"。
    // 目前这道拒绝落在**完成**那一步（HTTP 400「token 未冻结明文摘要」），
    // 也就是客户端会先白传一遍正文；放到 prepare 就拒会更省事，但安全性是一样的。
    let refused = upload_blob(&token_b, &seal_with_token(&token_b, &blob_b)?).await;
    let err = refused
        .err()
        .ok_or("🔴 没有冻结明文身份的上传竟然成功了：verify-before-publish 被绕过")?
        .to_string();
    assert!(
        err.contains("未冻结明文摘要"),
        "🔴 期望因为缺少冻结的明文身份被拒，实际={err}"
    );
    println!("   已按预期拒绝：{err}");

    // 不同明文 → 不同物理文件（这次带上摘要，走正常路径）。
    let digest_b = sha256_hex(&blob_b);
    let token_b2 = request_token(&sdk, blob_b.len(), Some(&digest_b)).await?;
    let file_b = upload_blob(&token_b2, &seal_with_token(&token_b2, &blob_b)?).await?;
    let url_b = file_url(&file_b)?;
    assert_ne!(url_b, url_a, "🔴 不同内容必须是不同的物理文件");
    println!("   file_id={} url={}", file_id(&file_b)?, url_b);

    // ---------------------------------------------------------------- 场景 5
    println!("\n7) 重新封装同一份明文 → 密文是另一串字节，但必须**仍然命中**");
    let resealed = seal_with_token(&token_a, &blob_a)?;
    let first_sealed = seal_with_token(&token_a, &blob_a)?;
    assert_ne!(
        resealed, first_sealed,
        "🔴 每块都用新 nonce，同一份明文封两次不该得到同一串字节；\
         真相等说明 nonce 复用了，那是可以直接解密的严重缺陷"
    );
    let token_c = request_token(&sdk, blob_a.len(), Some(&digest_a)).await?;
    assert_eq!(
        token_c["already_exists"].as_bool(),
        Some(true),
        "🔴 判重键是明文摘要。这里 miss 就说明又退回按密文判重了——\
         那样同一份内容永远命中不了"
    );

    // ---------------------------------------------------------------- 场景 6
    println!("\n8) 换一个毫不相干的用户 → 仍然命中，且不泄露别人的 file_id");
    let other_dir = std::env::temp_dir().join(format!(
        "privchat-dedup-other-{}-{}",
        ts,
        std::process::id()
    ));
    std::fs::create_dir_all(&other_dir)?;
    let other = PrivchatSdk::new(sdk_config(&host, tcp_port, &other_dir));
    other.connect().await?;
    let other_name = format!("dedup_other_{}", ts);
    let other_uid = sign_in(&other, "B", other_name).await?;
    let _ = other_uid;
    let token_x = request_token(&other, blob_a.len(), Some(&digest_a)).await?;
    assert_eq!(
        token_x["already_exists"].as_bool(),
        Some(true),
        "🔴 跨用户秒传是这套设计要的结果：判重键是内容，不是谁传的"
    );
    assert!(
        token_x["file_id"].as_str().unwrap_or_default().is_empty(),
        "🔴 探测只回答「在不在」，不许把别人的 file_id 递出去；实际={}",
        token_x["file_id"]
    );
    let claimed_x = claim_existing(&other, &token_x, &digest_a).await?;
    assert_eq!(
        file_url(&claimed_x)?,
        url_a,
        "🔴 另一个用户取用的必须是同一个物理文件"
    );
    assert_ne!(
        file_id(&claimed_x)?,
        id_a,
        "🔴 但要有自己的一条记录，不是把源记录递回来"
    );
    other.disconnect().await?;

    sdk.disconnect().await?;
    println!("\n✅ 秒传真链路全部通过");
    Ok(())
}

// ---------------------------------------------------------------------------

/// 两个客户端连同一台服务端，但各自独立的本地目录——跨用户秒传要的是两个
/// 毫不相干的账号，共用 data_dir 会让本地缓存把结果搅浑。
fn sdk_config(host: &str, tcp_port: u16, data_dir: &std::path::Path) -> PrivchatConfig {
    // tcp:// 是 TLS-only 且缺 pin 直接拒连（裸 IP + 自签部署下没有别的身份来源），
    // 所以 pin 必须显式给：
    //   PRIVCHAT_SPKI_PINS=$(openssl x509 -in certs/server.crt -pubkey -noout \
    //     | openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | base64)
    let spki_pins: Vec<String> = std::env::var("PRIVCHAT_SPKI_PINS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    PrivchatConfig {
        endpoints: vec![ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: host.to_string(),
            port: tcp_port,
            path: None,
            use_tls: true,
        }],
        connection_timeout_secs: 30,
        data_dir: data_dir.to_string_lossy().to_string(),
        spki_pins,
    }
}

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

/// 申请上传 token。申报的是**明文**大小与**明文**摘要——判重键就是它。
async fn request_token(
    sdk: &PrivchatSdk,
    plaintext_size: usize,
    plaintext_sha256: Option<&str>,
) -> BoxResult<Value> {
    let mut body = json!({
        "user_id": 0,
        "filename": "dedup-probe.bin",
        "plaintext_size": plaintext_size as i64,
        "mime_type": "application/octet-stream",
        "file_type": "file",
        "business_type": "message",
    });
    if let Some(d) = plaintext_sha256 {
        body["plaintext_sha256"] = json!(d);
    }
    Ok(serde_json::from_str(
        &sdk.rpc_call("file/request_upload_token".to_string(), body.to_string())
            .await?,
    )?)
}

/// 用 token 下发的密钥与块大小封装明文。
///
/// 🔴 封装只能发生在**拿到 token 之后**：密钥和块大小都是响应带回来的，
/// 提前封没有密钥可用。这也是"复用已有密文"那条捷径不存在的原因。
fn seal_with_token(token: &Value, plaintext: &[u8]) -> BoxResult<Vec<u8>> {
    let key_b64 = token["attachment_key"]["key"]
        .as_str()
        .ok_or("token response missing attachment_key.key")?;
    let key_id = token["attachment_key"]["key_id"]
        .as_u64()
        .ok_or("token response missing attachment_key.key_id")? as u8;
    let chunk = token["chunk_plain_size"]
        .as_u64()
        .ok_or("token response missing chunk_plain_size")? as u32;
    let key = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        key_b64,
    )?;
    let sealed = privchat_protocol::attachment_crypto::encrypt_attachment_with_chunk_size(
        plaintext, &key, key_id, chunk,
    )?;
    // 服务端按 token 里签好的 total_size 收字节，对不上直接拒绝。
    let expected = token["total_size"]
        .as_u64()
        .ok_or("token response missing total_size")?;
    if sealed.len() as u64 != expected {
        return Err(format!(
            "sealed length {} does not match the signed total_size {}",
            sealed.len(),
            expected
        )
        .into());
    }
    Ok(sealed)
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
