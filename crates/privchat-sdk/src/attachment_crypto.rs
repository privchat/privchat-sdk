// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Licensed under the Apache License, Version 2.0 (the "License").

//! 附件加密 v1（ATTACHMENT_ENCRYPTION_SPEC）：AES-256-GCM 整文件加密。
//!
//! - `blob = nonce(12B) || ciphertext || tag(16B)`（aes-gcm 的 ciphertext 已含尾部 16B tag）。
//! - `cek = base64url(no-pad)` 的 32 字节随机密钥。
//! - 与 WebCrypto `AES-GCM` 字节兼容（同样 ct||tag），App/Web 互解。
//! - **CEK 绝不进日志。**

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;

pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;
pub const CEK_LEN: usize = 32;
/// 最小密文 blob：12 nonce + 16 tag（空明文边界）。
pub const MIN_BLOB_LEN: usize = NONCE_LEN + TAG_LEN;

/// 加密明文 → `(blob, cek_base64url)`。CSPRNG 生成 cek + nonce。
/// blob 直接上传对象存储；cek 走 file 表 / 鉴权后的 get_url 响应。
pub fn encrypt_attachment(plaintext: &[u8]) -> Result<(Vec<u8>, String), String> {
    let mut cek = [0u8; CEK_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    let mut rng = rand::thread_rng();
    rng.fill_bytes(&mut cek);
    rng.fill_bytes(&mut nonce_bytes);

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&cek));
    let ct_with_tag = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|_| "attachment encrypt failed".to_string())?;

    let mut blob = Vec::with_capacity(NONCE_LEN + ct_with_tag.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ct_with_tag);

    let cek_b64 = URL_SAFE_NO_PAD.encode(cek);
    Ok((blob, cek_b64))
}

/// 解密 `blob (nonce||ct||tag)` + `cek_base64url` → 明文。
/// GCM tag 校验失败（错 key / 篡改）返回 Err。
pub fn decrypt_attachment(blob: &[u8], cek_b64: &str) -> Result<Vec<u8>, String> {
    if blob.len() < MIN_BLOB_LEN {
        return Err(format!(
            "attachment blob too short: {} < {}",
            blob.len(),
            MIN_BLOB_LEN
        ));
    }
    let cek = URL_SAFE_NO_PAD
        .decode(cek_b64.as_bytes())
        .map_err(|_| "cek is not valid base64url".to_string())?;
    if cek.len() != CEK_LEN {
        return Err(format!("cek must be {} bytes, got {}", CEK_LEN, cek.len()));
    }
    let (nonce_bytes, ct_with_tag) = blob.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&cek));
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ct_with_tag)
        .map_err(|_| "attachment decrypt/auth failed".to_string())
}

// ---------------------------------------------------------------------------
// v2：全站统一密钥
// ---------------------------------------------------------------------------

/// v2 blob 头：`magic(2B) || version(1B) || key_id(1B)`，其后是 `nonce || ct || tag`。
///
/// 头自描述，解密不依赖任何服务端字段——密钥轮换后老对象照样解得开。
const V2_MAGIC: [u8; 2] = *b"PC";
const V2_VERSION: u8 = 2;
const V2_HEADER_LEN: usize = 4;
pub const MIN_V2_BLOB_LEN: usize = V2_HEADER_LEN + NONCE_LEN + TAG_LEN;

/// v2 密文比明文固定多这么多字节（头 + nonce + tag）。
///
/// 🔴 上传 token 要签下**密文**的字节数，而密钥要等 token 回来才拿得到——
/// 先有鸡还是先有蛋。因为这个增量是常数，客户端可以在拿到密钥之前就把密文大小
/// 算准，于是 `prepare` 能先发出去、密钥随 token 回来、再加密上传。
pub const V2_OVERHEAD: usize = V2_HEADER_LEN + NONCE_LEN + TAG_LEN;

/// 给定明文长度，v2 密文的确切字节数。
pub fn v2_sealed_len(plaintext_len: usize) -> usize {
    plaintext_len + V2_OVERHEAD
}

/// 用全站密钥加密。
///
/// 威胁模型是**对象存储服务商**：不少厂商会拿用户上传的图片视频去训练。所以明文和
/// 密钥都不能出现在服务端或 S3，加解密只在客户端做。用户之间不靠这把密钥隔离——
/// 那由鉴权（`resolve_attachment_access`）和不可枚举的内容寻址路径负责。
///
/// 🔴 **nonce 必须逐次随机，绝不能固定或按内容派生。** 现在全站共用一把密钥，
/// AES-GCM 在同一密钥下重用 nonce 会直接崩：泄露两条明文的异或，并且可以伪造。
/// per-file 密钥时代这个错误还有密钥隔离兜底，现在没有了。想按内容去重必须换
/// AES-GCM-SIV，而 WebCrypto 不支持 SIV——见 ATTACHMENT_ENCRYPTION_SPEC。
pub fn encrypt_attachment_v2(plaintext: &[u8], key: &[u8], key_id: u8) -> Result<Vec<u8>, String> {
    if key.len() != CEK_LEN {
        return Err(format!("site key must be {CEK_LEN} bytes, got {}", key.len()));
    }
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let ct_with_tag = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|_| "attachment encrypt failed".to_string())?;

    let mut blob = Vec::with_capacity(MIN_V2_BLOB_LEN + plaintext.len());
    blob.extend_from_slice(&V2_MAGIC);
    blob.push(V2_VERSION);
    blob.push(key_id);
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ct_with_tag);
    Ok(blob)
}

/// 读出 v2 blob 声明的 key_id，用于挑选密钥（轮换期会同时存在两代对象）。
pub fn v2_key_id(blob: &[u8]) -> Option<u8> {
    if blob.len() >= MIN_V2_BLOB_LEN && blob[0..2] == V2_MAGIC && blob[2] == V2_VERSION {
        Some(blob[3])
    } else {
        None
    }
}

/// 用全站密钥解密 v2 blob。
pub fn decrypt_attachment_v2(blob: &[u8], key: &[u8]) -> Result<Vec<u8>, String> {
    if blob.len() < MIN_V2_BLOB_LEN {
        return Err(format!(
            "attachment blob too short: {} < {MIN_V2_BLOB_LEN}",
            blob.len()
        ));
    }
    if blob[0..2] != V2_MAGIC || blob[2] != V2_VERSION {
        return Err("not a v2 attachment blob".to_string());
    }
    if key.len() != CEK_LEN {
        return Err(format!("site key must be {CEK_LEN} bytes, got {}", key.len()));
    }
    let (nonce_bytes, ct_with_tag) = blob[V2_HEADER_LEN..].split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ct_with_tag)
        .map_err(|_| "attachment decrypt/auth failed".to_string())
}

/// 下载完成后按加密信息把 blob 还原成明文（run_download / thumbnail 下载统一调用）。
///
/// - `version=0`（或上层视为缺失时传 0）→ legacy 明文，原样返回。
/// - `version=1` → `cek` **必须存在**，blob 校验 + AES-GCM 解密；缺 cek 或解密失败一律 Err，
///   **绝不 fallback 成明文**（否则会把密文当图片写入，UI 显示坏图并掩盖错误）。
pub fn decrypt_downloaded_attachment_bytes(
    encryption_version: i32,
    cek: Option<&str>,
    blob: &[u8],
) -> Result<Vec<u8>, String> {
    match encryption_version {
        0 => Ok(blob.to_vec()),
        1 => {
            let cek = cek
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "encryption_version=1 but cek missing".to_string())?;
            decrypt_attachment(blob, cek)
        }
        v => Err(format!("unsupported encryption_version: {v}")),
    }
}

/// v2 版本的还原入口：密钥由调用方按 blob 里的 key_id 选出。
///
/// `version=0` 仍是 legacy 明文；`version=1` 走 per-file CEK（历史对象）；
/// `version=2` 用全站密钥。**任何一条都不许在失败时回落成明文**——那会把密文
/// 当图片写进缓存，UI 显示坏图，同时把真实错误藏起来。
pub fn decrypt_downloaded_attachment_bytes_v2(
    encryption_version: i32,
    cek: Option<&str>,
    site_key: Option<&[u8]>,
    blob: &[u8],
) -> Result<Vec<u8>, String> {
    match encryption_version {
        2 => {
            let key = site_key.ok_or_else(|| {
                "encryption_version=2 but no site key available".to_string()
            })?;
            decrypt_attachment_v2(blob, key)
        }
        other => decrypt_downloaded_attachment_bytes(other, cek, blob),
    }
}
// 单测见 tests/attachment_crypto_test.rs（集成测试，绕开 lib 内不相关的 test fixture）。
