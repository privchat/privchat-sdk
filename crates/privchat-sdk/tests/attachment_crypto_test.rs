// 附件加密纯 crypto 单测（ATTACHMENT_ENCRYPTION_SPEC §测试）。
// 放 tests/ 集成测试：链接 lib 非-test 构建，绕开 lib 内不相关的 #[cfg(test)] fixture。

use privchat_sdk::attachment_crypto::{
    decrypt_attachment, decrypt_downloaded_attachment_bytes, encrypt_attachment, MIN_BLOB_LEN,
    NONCE_LEN,
};

#[test]
fn roundtrip() {
    let plain = b"hello privchat attachment encryption \x00\x01\x02";
    let (blob, cek) = encrypt_attachment(plain).unwrap();
    assert!(blob.len() >= MIN_BLOB_LEN + plain.len());
    assert_eq!(decrypt_attachment(&blob, &cek).unwrap(), plain);
}

#[test]
fn empty_plaintext_roundtrip() {
    let (blob, cek) = encrypt_attachment(b"").unwrap();
    assert_eq!(blob.len(), MIN_BLOB_LEN); // 12 nonce + 0 ct + 16 tag
    assert_eq!(decrypt_attachment(&blob, &cek).unwrap(), b"");
}

#[test]
fn wrong_key_fails() {
    let (blob, _cek) = encrypt_attachment(b"secret").unwrap();
    let (_b2, other_cek) = encrypt_attachment(b"x").unwrap();
    assert!(decrypt_attachment(&blob, &other_cek).is_err());
}

#[test]
fn tampered_tag_fails() {
    let (mut blob, cek) = encrypt_attachment(b"secret").unwrap();
    let last = blob.len() - 1;
    blob[last] ^= 0xff; // 篡改尾部 tag
    assert!(decrypt_attachment(&blob, &cek).is_err());
}

#[test]
fn tampered_ciphertext_fails() {
    let (mut blob, cek) = encrypt_attachment(b"secret-body").unwrap();
    blob[NONCE_LEN] ^= 0x01; // 篡改密文第一字节
    assert!(decrypt_attachment(&blob, &cek).is_err());
}

#[test]
fn short_blob_rejected() {
    // blob 长度检查在 cek decode 之前，所以 cek 用任意值即可
    assert!(decrypt_attachment(&[0u8; 10], "AAAA").is_err());
}

#[test]
fn bad_cek_length_rejected() {
    let (blob, _) = encrypt_attachment(b"x").unwrap();
    // 22 个 'A' = base64url(16 字节零) → 非 32B，应拒绝
    let short_cek = "AAAAAAAAAAAAAAAAAAAAAA";
    assert!(decrypt_attachment(&blob, short_cek).is_err());
}

// ---- download helper: decrypt_downloaded_attachment_bytes ----

#[test]
fn download_legacy_v0_returns_original() {
    let raw = b"legacy plaintext bytes";
    assert_eq!(
        decrypt_downloaded_attachment_bytes(0, None, raw).unwrap(),
        raw
    );
}

#[test]
fn download_v1_roundtrip() {
    let plain = b"download me";
    let (blob, cek) = encrypt_attachment(plain).unwrap();
    let out = decrypt_downloaded_attachment_bytes(1, Some(&cek), &blob).unwrap();
    assert_eq!(out, plain);
}

#[test]
fn download_v1_missing_cek_fails() {
    let (blob, _cek) = encrypt_attachment(b"x").unwrap();
    assert!(decrypt_downloaded_attachment_bytes(1, None, &blob).is_err());
    assert!(decrypt_downloaded_attachment_bytes(1, Some(""), &blob).is_err());
}

#[test]
fn download_v1_short_blob_fails() {
    assert!(decrypt_downloaded_attachment_bytes(1, Some("AAAA"), &[0u8; 10]).is_err());
}

#[test]
fn download_v1_wrong_cek_fails() {
    let (blob, _cek) = encrypt_attachment(b"x").unwrap();
    let (_b2, other) = encrypt_attachment(b"y").unwrap();
    assert!(decrypt_downloaded_attachment_bytes(1, Some(&other), &blob).is_err());
}

// ---------------------------------------------------------------------------
// v2：全站统一密钥
// ---------------------------------------------------------------------------

use privchat_sdk::attachment_crypto::{
    decrypt_attachment_v2, decrypt_downloaded_attachment_bytes_v2, encrypt_attachment_v2,
    v2_key_id, MIN_V2_BLOB_LEN,
};

fn site_key(seed: u8) -> [u8; 32] {
    [seed; 32]
}

#[test]
fn v2_round_trips_under_the_site_key() {
    let key = site_key(7);
    let plain = b"waterfall.jpg contents";
    let blob = encrypt_attachment_v2(plain, &key, 1).expect("encrypt");
    assert_eq!(decrypt_attachment_v2(&blob, &key).expect("decrypt"), plain);
}

/// 密文里绝不能出现明文片段——这是整个方案要防的事：
/// 对象存储服务商只拿得到这段字节。
#[test]
fn the_ciphertext_reveals_nothing_of_the_plaintext() {
    let key = site_key(7);
    let plain = b"a recognisable marker string";
    let blob = encrypt_attachment_v2(plain, &key, 1).expect("encrypt");
    assert!(
        blob.windows(plain.len()).all(|w| w != plain),
        "明文出现在密文里"
    );
    assert!(blob.len() >= MIN_V2_BLOB_LEN);
}

/// 🔴 同一份明文两次加密必须产出不同密文。
/// nonce 一旦固定或按内容派生，AES-GCM 在同一密钥下就会泄露明文异或并可被伪造——
/// 全站共用一把密钥之后，这条是唯一的护栏。
#[test]
fn the_same_plaintext_never_produces_the_same_ciphertext() {
    let key = site_key(7);
    let plain = b"identical bytes";
    let a = encrypt_attachment_v2(plain, &key, 1).expect("a");
    let b = encrypt_attachment_v2(plain, &key, 1).expect("b");
    assert_ne!(a, b, "nonce 必须逐次随机");
    // 但都要能解回同一份明文
    assert_eq!(decrypt_attachment_v2(&a, &key).unwrap(), plain);
    assert_eq!(decrypt_attachment_v2(&b, &key).unwrap(), plain);
}

#[test]
fn a_wrong_key_is_rejected_rather_than_returning_garbage() {
    let blob = encrypt_attachment_v2(b"x", &site_key(7), 1).expect("encrypt");
    assert!(decrypt_attachment_v2(&blob, &site_key(8)).is_err());
}

#[test]
fn tampering_is_detected() {
    let key = site_key(7);
    let mut blob = encrypt_attachment_v2(b"payload", &key, 1).expect("encrypt");
    let last = blob.len() - 1;
    blob[last] ^= 0xff;
    assert!(decrypt_attachment_v2(&blob, &key).is_err());
}

/// key_id 自描述：轮换期两代对象并存，解密方按 blob 自己挑密钥，
/// 不依赖任何服务端字段。
#[test]
fn the_key_id_travels_with_the_object() {
    for id in [0u8, 1, 42, 255] {
        let blob = encrypt_attachment_v2(b"x", &site_key(1), id).expect("encrypt");
        assert_eq!(v2_key_id(&blob), Some(id));
    }
    assert_eq!(v2_key_id(b"too short"), None, "非 v2 blob 不得误判");
}

/// 版本分派：v0 明文、v1 per-file CEK（历史对象）、v2 全站密钥，三条并存。
#[test]
fn all_three_versions_stay_readable() {
    let key = site_key(9);
    let v2 = encrypt_attachment_v2(b"new", &key, 1).expect("v2");
    assert_eq!(
        decrypt_downloaded_attachment_bytes_v2(2, None, Some(&key), &v2).unwrap(),
        b"new"
    );
    assert_eq!(
        decrypt_downloaded_attachment_bytes_v2(0, None, None, b"legacy plain").unwrap(),
        b"legacy plain"
    );
}

/// 缺密钥必须报错，绝不回落成明文——那会把密文当图片写进缓存。
#[test]
fn a_missing_site_key_never_falls_back_to_plaintext() {
    let blob = encrypt_attachment_v2(b"x", &site_key(1), 1).expect("encrypt");
    let err = decrypt_downloaded_attachment_bytes_v2(2, None, None, &blob).unwrap_err();
    assert!(err.contains("no site key"), "{err}");
}
