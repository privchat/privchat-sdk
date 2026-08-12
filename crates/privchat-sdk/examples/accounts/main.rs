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

mod account_manager;
mod coordinator;
mod phases;
mod types;

use account_manager::MultiAccountManager;
use coordinator::TestCoordinator;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

#[tokio::main]
async fn main() -> BoxResult<()> {
    // Admin/metrics phases use reqwest against the local server (127.0.0.1).
    // reqwest honors the macOS system HTTP proxy (e.g. Clash on :7890), which
    // returns 502 for loopback admin calls; curl bypasses it, hence the mismatch.
    // Never proxy localhost smoke traffic — default a loopback no_proxy unless
    // the caller already set one.
    // 🔴 合并进**已有**的值，而不是「两个都没设才写」：只要环境里已经有一个
    // `NO_PROXY=localhost`，缺失的网段就永远补不上，附件上传照样走代理。
    ensure_proxy_bypass();

    println!("\nPrivChat SDK Multi-Account Example (accounts)");
    println!("================================================");
    println!("Phases: full business interoperability + local-first naming/cache rules + room + channel-state-resume smoke + unread-resume strict + admin push/revoke + platform bot-followed smoke + fsync friend lifecycle + system-user group-reject + system-user message-dispatch smoke\n");

    let started = std::time::Instant::now();
    let mut manager = MultiAccountManager::new().await?;

    let alice = manager.account_config("alice")?;
    let bob = manager.account_config("bob")?;
    let charlie = manager.account_config("charlie")?;
    println!("Accounts:");
    println!("  alice   => {} (uid={})", alice.username, alice.user_id);
    println!("  bob     => {} (uid={})", bob.username, bob.user_id);
    println!(
        "  charlie => {} (uid={})",
        charlie.username, charlie.user_id
    );
    println!("Data dir: {}\n", manager.base_dir.display());

    let mut coordinator = TestCoordinator::new();
    coordinator.run_all(&mut manager).await?;

    let summary = coordinator.summary(started.elapsed());
    println!("\nSummary");
    println!("-------");
    println!("total phases : {}", summary.total);
    println!("passed       : {}", summary.passed);
    println!("failed       : {}", summary.failed);
    println!("duration     : {:.2}s", summary.duration.as_secs_f64());

    manager.cleanup().await?;

    if summary.failed > 0 {
        return Err(boxed_err(format!("{} phase(s) failed", summary.failed)));
    }

    Ok(())
}

/// 让本机 HTTP 代理放过 loopback 和内网。
///
/// 两件事都会踩到：admin/metrics 走 127.0.0.1，而真机调试时服务端广播的上传地址是
/// 局域网 IP（config.toml `[file]` base urls）。被代理拦下的表现是 502 / 连不上，
/// 看起来像产品坏了——curl 却是好的，因为 curl 不读这两个变量。
fn ensure_proxy_bypass() {
    const NEEDED: [&str; 6] = [
        "127.0.0.1",
        "localhost",
        "::1",
        "192.168.0.0/16",
        "10.0.0.0/8",
        "172.16.0.0/12",
    ];
    let existing = std::env::var("NO_PROXY")
        .ok()
        .or_else(|| std::env::var("no_proxy").ok())
        .unwrap_or_default();
    let mut entries: Vec<String> = existing
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    for want in NEEDED {
        if !entries.iter().any(|e| e == want) {
            entries.push(want.to_string());
        }
    }
    let merged = entries.join(",");
    // 大小写两个都写：不同 HTTP 客户端读的不是同一个。
    std::env::set_var("NO_PROXY", &merged);
    std::env::set_var("no_proxy", &merged);
}

fn boxed_err(msg: impl Into<String>) -> BoxError {
    Box::new(std::io::Error::other(msg.into()))
}
