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

//! 分片上传的真链路验证：真 SDK、真 RPC、真 HTTP 数据面、真服务端、真对象存储。
//!
//! 已有的 `resumable_upload_e2e_test` 直接打 axum Router，验的是服务端内部那一段。
//! 这里补的是它证明不了的部分：**客户端经由网关 RPC 申请分片 token，再按服务端
//! 下发的几何把密文一段段传上去。** 中间凡是两边对几何理解不一致的地方——块大小、
//! 密文总长、offset 网格——都只会在这条路上暴露。
//!
//! 验的是：
//!
//! 1. 密文超过阈值时服务端**确实**下发分片方案（没下发就等于这条路根本没走）。
//! 2. 客户端按 token 下发的密钥与块大小封装，长度与 `total_size` 逐字节吻合。
//! 3. 分段上传到一半时 `files/status` 报的已确认区间，与真正传出去的字节吻合——
//!    断点续传要的就是这个，"传完能成功"证明不了它。
//! 4. 传完 `files/complete` 落库，`file/get_url` 能取回。
//! 5. 同一份明文再走一次 → 秒传命中，不再传任何正文。
//!
//! 跑法（需要本机 server 起着）：
//! ```bash
//! export PRIVCHAT_SPKI_PINS=$(openssl x509 -in ../privchat-server/certs/server.crt \
//!   -pubkey -noout | openssl pkey -pubin -outform der \
//!   | openssl dgst -sha256 -binary | base64)
//! cargo run -p privchat-sdk --example chunked
//! ```

use std::time::{SystemTime, UNIX_EPOCH};

use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, TransportProtocol};
use serde_json::{json, Value};
use sha2::Digest as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

/// 明文取 300 KiB：密文必然超过服务端 64 KiB 的分片阈值，又不至于让这个例子跑很久。
const PLAINTEXT_LEN: usize = 300 * 1024;

#[tokio::main]
async fn main() -> BoxResult<()> {
    let host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let tcp_port: u16 = std::env::var("PRIVCHAT_TCP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9001);

    let ts = now_millis();
    let data_dir =
        std::env::temp_dir().join(format!("privchat-chunked-{}-{}", ts, std::process::id()));
    std::fs::create_dir_all(&data_dir)?;

    let sdk = PrivchatSdk::new(sdk_config(&host, tcp_port, &data_dir));

    println!("1) connect + register + authenticate");
    sdk.connect().await?;
    let username = format!("chunked_{}{}", ts % 100000, std::process::id());
    let login = sdk
        .register(username, "password123".to_string(), uuid_like())
        .await?;
    sdk.authenticate(login.user_id, login.token.clone(), login.device_id.clone())
        .await?;
    println!("   user_id={}", login.user_id);

    // ---------------------------------------------------------------- 场景 1
    println!("\n2) 申请分片 token → 服务端必须下发分片方案");
    let plaintext = random_blob(ts, PLAINTEXT_LEN);
    let digest = sha256_hex(&plaintext);
    let token = request_chunked_token(&sdk, plaintext.len(), &digest, false).await?;
    assert_eq!(
        token["already_exists"].as_bool(),
        Some(false),
        "🔴 这串明文服务端没见过，预检必须 miss"
    );
    if std::env::var("PRIVCHAT_DUMP").is_ok() {
        println!("   token = {}", serde_json::to_string_pretty(&token)?);
    }
    // 🔴 分片响应是扁平的：几何直接在顶层（`base_unit` / `transport`），
    // 嵌套的 `upload_plan` 是**整包**那条响应的字段。拿错地方会让这个例子
    // 悄悄退化成"没验到"。
    let base_unit = token["base_unit"]
        .as_u64()
        .ok_or("🔴 分片响应没有 base_unit：会话没建起来，这条例子什么都没验到")?
        as usize;
    assert!(base_unit > 0, "🔴 base_unit 必须为正");
    let transport = token["transport"].as_str().unwrap_or("proxy_offset_v1");
    assert_eq!(
        transport, "proxy_offset_v1",
        "🔴 本机没配 S3，数据面只能是内置上传服务"
    );
    let total_size = token["total_size"]
        .as_u64()
        .ok_or("token response missing total_size")?;
    assert!(
        total_size > base_unit as u64,
        "🔴 密文 {total_size} 字节没超过 base_unit {base_unit}，一段就传完了——\
         那验的还是整包，不是分片"
    );
    println!(
        "   base_unit={base_unit} transport={transport} 预计 {} 段",
        total_size.div_ceil(base_unit as u64)
    );

    // ---------------------------------------------------------------- 场景 2
    println!("\n3) 按 token 下发的几何封装 → 长度必须等于服务端签下的 total_size");
    let sealed = seal_with_token(&token, &plaintext)?;
    assert_eq!(
        sealed.len() as u64,
        total_size,
        "🔴 两边分块几何不一致：客户端封出 {} 字节，服务端按 {} 字节收",
        sealed.len(),
        total_size
    );
    println!("   plaintext={} sealed={}", plaintext.len(), sealed.len());

    let upload_token = token["upload_token"]
        .as_str()
        .ok_or("token response missing upload_token")?;
    let upload_url = token["upload_url"]
        .as_str()
        .ok_or("token response missing upload_url")?;
    let base = upload_base(upload_url);

    // ---------------------------------------------------------------- 场景 3
    println!("\n4) 传一半 → status 报的已确认区间必须与真正传出去的字节吻合");
    let half = (sealed.len() / 2 / base_unit).max(1) * base_unit;
    let mut offset = 0usize;
    while offset < half {
        let end = (offset + base_unit).min(half);
        put_chunk(&base, upload_token, offset, &sealed[offset..end]).await?;
        offset = end;
    }
    let status = get_status(&base, upload_token).await?;
    let confirmed = confirmed_bytes(&status);
    assert_eq!(
        confirmed, offset as u64,
        "🔴 服务端认下的字节数({confirmed})与实际传出去的({offset})对不上——\
         断点续传会从错误的位置接着传"
    );
    println!("   已传 {offset} / {}，status 确认 {confirmed}", sealed.len());

    // ---------------------------------------------------------------- 场景 4
    println!("\n5) 传完剩下的 → complete 落库，get_url 能取回");
    while offset < sealed.len() {
        let end = (offset + base_unit).min(sealed.len());
        put_chunk(&base, upload_token, offset, &sealed[offset..end]).await?;
        offset = end;
    }
    let done = complete(&base, upload_token).await?;
    let file_id = payload(&done)["file_id"]
        .as_str()
        .map(str::to_string)
        .or_else(|| payload(&done)["file_id"].as_u64().map(|n| n.to_string()))
        .ok_or_else(|| format!("complete 没有回 file_id: {done}"))?;
    let url = payload(&done)["file_url"]
        .as_str()
        .ok_or_else(|| format!("complete 没有回 file_url: {done}"))?
        .to_string();
    println!("   file_id={file_id} url={url}");

    let detail = get_url(&sdk, file_id.parse::<u64>()?).await?;
    assert_eq!(
        payload(&detail)["plaintext_sha256"].as_str(),
        Some(digest.as_str()),
        "🔴 get_url 回的明文摘要必须就是申请时冻结的那个"
    );

    // ---------------------------------------------------------------- 场景 5
    println!("\n6) 同一份明文再走一次 → 秒传命中，正文一个字节都不用再传");
    let again = request_chunked_token(&sdk, plaintext.len(), &digest, false).await?;
    assert_eq!(
        again["already_exists"].as_bool(),
        Some(true),
        "🔴 分片路径也必须吃秒传：判重键是明文摘要，与走哪条数据面无关"
    );
    assert!(
        again["upload_token"].as_str().unwrap_or_default().is_empty()
            || again["upload_token"].is_null(),
        "🔴 命中时不该再发上传凭据；实际={}",
        again["upload_token"]
    );

    sdk.disconnect().await?;
    println!("\n✅ 分片真链路全部通过");
    Ok(())
}

// ---------------------------------------------------------------------------

fn sdk_config(host: &str, tcp_port: u16, data_dir: &std::path::Path) -> PrivchatConfig {
    // tcp:// 是 TLS-only 且缺 pin 直接拒连，pin 必须显式给（见文件头跑法）。
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

async fn request_chunked_token(
    sdk: &PrivchatSdk,
    plaintext_size: usize,
    plaintext_sha256: &str,
    force_upload: bool,
) -> BoxResult<Value> {
    let body = json!({
        "file_type": "file",
        "business_type": "message",
        "plaintext_size": plaintext_size as i64,
        "plaintext_sha256": plaintext_sha256,
        "mime_type": "application/octet-stream",
        "filename": "chunked-probe.bin",
        "force_upload": force_upload,
        "supported_upload_transports": ["proxy_offset_v1"],
    });
    Ok(serde_json::from_str(
        &sdk.rpc_call(
            "file/request_chunked_upload_token".to_string(),
            body.to_string(),
        )
        .await?,
    )?)
}

/// 用 token 下发的密钥与块大小封装。客户端不自选几何——自选出来的密文长度对不上
/// token 里签好的 `total_size`，完成时必被拒。
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
    Ok(
        privchat_protocol::attachment_crypto::encrypt_attachment_with_chunk_size(
            plaintext, &key, key_id, chunk,
        )?,
    )
}

/// `upload_url` 就是 `/api/app/files`；chunk / status / complete 挂在它下面。
fn upload_base(upload_url: &str) -> String {
    upload_url.trim_end_matches('/').to_string()
}

async fn put_chunk(base: &str, token: &str, offset: usize, bytes: &[u8]) -> BoxResult<()> {
    let response = reqwest::Client::new()
        .put(format!("{base}/chunk?offset={offset}"))
        .header("X-Upload-Token", token)
        .header("X-Chunk-SHA256", sha256_hex(bytes))
        .body(bytes.to_vec())
        .send()
        .await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(format!("chunk at {offset} failed: status={status} body={body}").into());
    }
    Ok(())
}

async fn get_status(base: &str, token: &str) -> BoxResult<Value> {
    let response = reqwest::Client::new()
        .get(format!("{base}/status"))
        .header("X-Upload-Token", token)
        .send()
        .await?;
    let status = response.status();
    let body: Value = response.json().await?;
    if !status.is_success() {
        return Err(format!("status failed: status={status} body={body}").into());
    }
    Ok(payload(&body).clone())
}

/// status 报的已确认区间总字节数。
fn confirmed_bytes(status: &Value) -> u64 {
    status["received_ranges"]
        .as_array()
        .or_else(|| status["confirmed_ranges"].as_array())
        .map(|ranges| {
            ranges
                .iter()
                .filter_map(|r| r["length"].as_u64())
                .sum::<u64>()
        })
        .or_else(|| status["received_bytes"].as_u64())
        .unwrap_or(0)
}

async fn complete(base: &str, token: &str) -> BoxResult<Value> {
    let response = reqwest::Client::new()
        .post(format!("{base}/complete"))
        .header("X-Upload-Token", token)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await?;
    let status = response.status();
    let body: Value = response.json().await?;
    if !status.is_success() {
        return Err(format!("complete failed: status={status} body={body}").into());
    }
    Ok(body)
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

fn payload(envelope: &Value) -> &Value {
    envelope.get("data").filter(|v| !v.is_null()).unwrap_or(envelope)
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn random_blob(seed: u128, len: usize) -> Vec<u8> {
    let mut state = seed as u64 | 1;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
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
