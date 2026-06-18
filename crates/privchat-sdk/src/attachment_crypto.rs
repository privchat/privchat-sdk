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
// 单测见 tests/attachment_crypto_test.rs（集成测试，绕开 lib 内不相关的 test fixture）。
