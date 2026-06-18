// 附件加密纯 crypto 单测（ATTACHMENT_ENCRYPTION_SPEC §测试）。
// 放 tests/ 集成测试：链接 lib 非-test 构建，绕开 lib 内不相关的 #[cfg(test)] fixture。

use privchat_sdk::attachment_crypto::{
    decrypt_attachment, encrypt_attachment, MIN_BLOB_LEN, NONCE_LEN,
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
