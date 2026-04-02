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
    println!("\nPrivChat SDK Multi-Account Example (accounts)");
    println!("================================================");
    println!("Phases: full 32-phase business interoperability + local-first naming/cache rules + room + channel-state-resume smoke\n");

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

fn boxed_err(msg: impl Into<String>) -> BoxError {
    Box::new(std::io::Error::other(msg.into()))
}
