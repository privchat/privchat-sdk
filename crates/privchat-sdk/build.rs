//! 编译期生成 GIT_SHA、BUILD_TIMESTAMP 等元信息（供 version.rs 使用）
//! 以及 SDK_DB_VERSION（从 migrations/ 目录扫描 V{version}__*.sql 取最大版本号）

use std::env;
use std::fs;
use std::path::Path;
use vergen::EmitBuilder;

fn main() {
    let _ = EmitBuilder::builder()
        .build_timestamp()
        .git_sha(false)
        .emit();

    // 从 migrations/ 扫描 V{version}__*.sql，取最大版本号作为 SDK_DB_VERSION
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let migrations_dir = Path::new(&manifest_dir).join("migrations");
    let mut max_version: i64 = 0;
    if migrations_dir.exists() {
        if let Ok(entries) = fs::read_dir(&migrations_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Some(s) = name.to_str() {
                    // refinery 格式: V{version}__{name}.sql
                    if s.starts_with('V') && s.ends_with(".sql") {
                        let rest = &s[1..s.len() - 4]; // 去掉 V 和 .sql
                        if let Some(version_part) = rest.split("__").next() {
                            if let Ok(v) = version_part.parse::<i64>() {
                                if v > max_version {
                                    max_version = v;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    println!("cargo:rustc-env=SDK_DB_VERSION={}", max_version);
    println!("cargo:rerun-if-changed=migrations/");
}
