//! 文件控制面 pinning 的**确定性**验证：起一个真 TLS 服务端、用真证书、走真握手。
//!
//! 这里不依赖环境变量、不依赖本地已跑的服务、不依赖网络。E2E 用例可以在缺少
//! `PRIVCHAT_TEST_SPKI_PINS` 时跳过，但那样一来 CI 上就可能一条 TLS 路径都没验到——
//! 所以安全断言必须有一份自带服务端的版本。
//!
//! 覆盖：正确 pin 接受、错误 pin 拒绝、双 pin 轮换、空 pin 拒绝、公网明文拒绝。

use std::sync::Arc;

use base64::Engine;
use privchat_sdk::file_plane_http;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

/// 生成一张自签证书，返回 (cert_pem, key_pem, spki_pin_base64)。
fn cert_with_pin() -> (String, String, String) {
    let key = rcgen::KeyPair::generate().expect("keypair");
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .expect("params")
        .self_signed(&key)
        .expect("self-signed");

    // 与服务端 `openssl x509 -pubkey | openssl pkey -pubin -outform der | sha256 | base64`
    // 同一个口径：摘要取的是 SubjectPublicKeyInfo，不是整张证书。
    let pin = {
        use x509_parser::prelude::FromDer;
        let der = cert.der().to_vec();
        let (_, parsed) = x509_parser::certificate::X509Certificate::from_der(&der).expect("x509");
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(parsed.public_key().raw))
    };
    (cert.pem(), key.serialize_pem(), pin)
}

/// 起一个最小 HTTPS 服务端：握手成功就回一个 200，然后关闭。
async fn spawn_tls_server(cert_pem: String, key_pem: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    let key = rustls_pemfile::private_key(&mut std::io::Cursor::new(key_pem.as_bytes()))
        .expect("parse key")
        .expect("key present");
    let certs = rustls_pemfile::certs(&mut std::io::Cursor::new(cert_pem.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .expect("parse certs");

    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("versions")
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .expect("cert/key pair");

    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let _ = tls
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .await;
                    let _ = tls.shutdown().await;
                }
            });
        }
    });

    format!("https://localhost:{}", addr.port())
}

#[tokio::test]
async fn a_matching_pin_completes_a_real_handshake() {
    let (cert, key, pin) = cert_with_pin();
    let url = spawn_tls_server(cert, key).await;

    let client = file_plane_http::control_client(&url, &[pin]).expect("client builds");
    let response = client.get(&url).send().await.expect("request succeeds");
    assert!(response.status().is_success());
}

/// pinning 的全部意义就在这一条：换一张证书就必须连不上。
#[tokio::test]
async fn a_wrong_pin_refuses_the_connection() {
    let (cert, key, _) = cert_with_pin();
    let (_, _, other_pin) = cert_with_pin();
    let url = spawn_tls_server(cert, key).await;

    let client = file_plane_http::control_client(&url, &[other_pin]).expect("client builds");
    let err = client.get(&url).send().await.expect_err("must not connect");
    assert!(
        err.to_string().contains("certificate") || err.is_connect() || err.is_request(),
        "unexpected error: {err}"
    );
}

/// 轮换：带 current + next 的客户端要能连上两把密钥中的任意一把，
/// 这样服务端切换时不需要强制更新客户端。
#[tokio::test]
async fn either_pin_works_during_rotation() {
    let (cert_a, key_a, pin_a) = cert_with_pin();
    let (cert_b, key_b, pin_b) = cert_with_pin();
    let pins = vec![pin_a, pin_b];

    for (cert, key) in [(cert_a, key_a), (cert_b, key_b)] {
        let url = spawn_tls_server(cert, key).await;
        let client = file_plane_http::control_client(&url, &pins).expect("client builds");
        let response = client.get(&url).send().await.expect("either key is accepted");
        assert!(response.status().is_success());
    }
}

/// 缺 pin 不能退回系统信任链：那样配置错误会变成静默降级。
#[tokio::test]
async fn https_without_a_pin_is_refused_before_any_request() {
    let (cert, key, _) = cert_with_pin();
    let url = spawn_tls_server(cert, key).await;

    let err = file_plane_http::control_client(&url, &[]).expect_err("must refuse");
    assert!(format!("{err}").contains("no SPKI pin"), "{err}");
}

/// 公网明文必须拒绝——一次配置回退就会把 upload token 重新暴露出去。
/// 本地开发没有 TLS 可言，放行。
#[tokio::test]
async fn plaintext_is_allowed_only_for_local_hosts() {
    assert!(file_plane_http::control_client("http://127.0.0.1:9083/api/app", &[]).is_ok());
    let err = file_plane_http::control_client("http://106.55.63.153:9083/api/app", &[])
        .expect_err("public plaintext must be refused");
    assert!(format!("{err}").contains("plaintext"), "{err}");
}
