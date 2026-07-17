// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

//! PrivChat SDK — Channel Transfer example.
//!
//! Demonstrates the client→app RPC over `biz_type=19` (TransferRequest) /
//! `biz_type=20` (TransferResponse). The same wire envelope carries
//! bot/menu, game/poker, wallet/balance, etc. The SDK does NOT interpret
//! `body` / `data` bytes — encoding is decided by `route`.
//!
//! Spec:
//!   - `02-server/CHANNEL_TRANSFER_SPEC.md` v2.0
//!   - `07-application/CHANNEL_TRANSFER_DISPATCH_SPEC.md` v1.0
//!   - `07-application/BOT_INTERACTION_SPEC.md`
//!
//! Pre-conditions in your environment (any one will work for this demo):
//!   1. `privchat-application` running with a registered service handler
//!      for the route's service name. e.g. `bot/menu/get` is handled by the
//!      default `BotMenuTransferHandler` (service_id=9001).
//!   2. The connecting user has a direct channel to the target service, and
//!      `privchat_business_channel` has a binding row pointing
//!      `(channel_id) → service_id` with `dispatch_transfer_enabled = 1`.
//!
//! Without the above the demo will return:
//!   - 20901 ChannelNotBound — binding row missing / disabled
//!   - 20902 TransferServiceNotFound — route prefix doesn't match a service
//!   - 20903 ServiceDisabled — service.status = 0
//! These are all valid responses and prove the wire/dispatch path works.

use std::time::{SystemTime, UNIX_EPOCH};

use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, TransportProtocol};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

#[tokio::main]
async fn main() -> BoxResult<()> {
    println!("\nPrivChat SDK Channel Transfer Example");
    println!("=====================================");

    // ── 1) env / wiring ─────────────────────────────────────────────
    let host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let tcp_port = parse_env_u16("PRIVCHAT_TCP_PORT", 9001);
    let route =
        std::env::var("PRIVCHAT_TRANSFER_ROUTE").unwrap_or_else(|_| "bot/menu/get".to_string());
    let body = std::env::var("PRIVCHAT_TRANSFER_BODY")
        .unwrap_or_default()
        .into_bytes();
    let explicit_channel_id = std::env::var("PRIVCHAT_TRANSFER_CHANNEL_ID")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());

    let ts = now_millis();
    let data_dir = std::env::temp_dir().join(format!(
        "privchat-rust-transfer-{}-{}",
        ts,
        std::process::id()
    ));
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
        data_dir: path_to_string(&data_dir),
    });

    println!("1) connect");
    sdk.connect().await?;

    // ── 2) register-or-login ─────────────────────────────────────────
    let suffix = format!("{}{}", ts % 100000, std::process::id());
    let username = format!("transfer_{}", suffix);
    let password = "password123".to_string();
    let device_id = pseudo_uuid_v4_like();

    println!("2) register/login as {}", username);
    let login = match sdk
        .register(username.clone(), password.clone(), device_id.clone())
        .await
    {
        Ok(out) => out,
        Err(_) => sdk.login(username, password, device_id.clone()).await?,
    };
    println!("   user_id={}", login.user_id);

    println!("3) authenticate");
    sdk.authenticate(login.user_id, login.token, login.device_id)
        .await?;

    // ── 3) pick a channel ────────────────────────────────────────────
    let channel_id = if let Some(cid) = explicit_channel_id {
        println!("4) using PRIVCHAT_TRANSFER_CHANNEL_ID={}", cid);
        cid
    } else {
        // Boot-strap a tiny resync so local store has at least one channel.
        sdk.run_bootstrap_sync().await?;
        let channels = sdk.list_channels(10, 0).await?;
        match channels.first() {
            Some(ch) => {
                println!(
                    "4) using first channel from local store: id={} type={}",
                    ch.channel_id, ch.channel_type
                );
                ch.channel_id
            }
            None => {
                println!("4) no channel in local store; set PRIVCHAT_TRANSFER_CHANNEL_ID");
                println!("   (you need a direct channel with the bot/service user first)");
                sdk.disconnect().await?;
                return Ok(());
            }
        }
    };

    // ── 4) the actual transfer call ──────────────────────────────────
    println!(
        "5) transfer  channel_id={} route='{}' body_len={}",
        channel_id,
        route,
        body.len()
    );
    let reply = sdk.transfer(channel_id, route.clone(), body, 5_000).await?;

    println!("--- TransferResponse ---");
    println!("  request_id  : {}", reply.request_id);
    println!("  channel_id  : {}", reply.channel_id);
    println!("  code        : {}", reply.code);
    println!("  message     : {}", reply.message);
    println!("  data_len    : {}", reply.data.len());
    if !reply.data.is_empty() {
        // Try to print as UTF-8 (most routes use JSON); fall back to hex preview.
        match std::str::from_utf8(&reply.data) {
            Ok(s) if s.is_ascii() || s.chars().all(|c| !c.is_control() || c == '\n') => {
                let preview: String = s.chars().take(800).collect();
                println!("  data (utf8) : {}", preview);
            }
            _ => {
                let hex: String = reply
                    .data
                    .iter()
                    .take(64)
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!(
                    "  data (hex)  : {}{}",
                    hex,
                    if reply.data.len() > 64 { " …" } else { "" }
                );
            }
        }
    }

    // Quick explanation of the result for the operator running the demo.
    println!();
    match reply.code {
        0 => println!("✓ OK — wire + relay + dispatch + handler all green."),
        20900 => println!("× ChannelNotSubscribed — subscribe the channel first."),
        20901 => println!(
            "× ChannelNotBound — no privchat_business_channel row for channel {} (or dispatch_transfer_enabled=0).",
            channel_id
        ),
        20902 => println!(
            "× TransferServiceNotFound — route prefix '{}' doesn't match any registered service.",
            route.split('/').next().unwrap_or("")
        ),
        20903 => println!("× TransferServiceDisabled — service.status = 0."),
        20904 => println!("× TransferCallbackFailed — external callback URL unreachable."),
        c if c >= 20000 => println!("× business code {} — handler returned this; not a framework error.", c),
        c => println!("× code {} — see ERROR_CODE_SPEC for segment ownership.", c),
    }

    println!("\n6) disconnect");
    sdk.disconnect().await?;

    println!("\nDone");
    Ok(())
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn parse_env_u16(name: &str, default: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn pseudo_uuid_v4_like() -> String {
    // Reuses the same pseudo-uuid pattern as examples/basic — good enough
    // for demo device_id; not RFC-compliant.
    let t = now_millis();
    let p = std::process::id() as u128;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (t >> 32) as u32,
        (t & 0xffff) as u16,
        (p & 0xffff) as u16,
        ((t ^ p) & 0xffff) as u16,
        (t ^ (p << 32)) as u64 & 0xffff_ffff_ffff,
    )
}

fn path_to_string(p: &std::path::Path) -> String {
    p.to_string_lossy().to_string()
}
