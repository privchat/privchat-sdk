#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ -z "${JAVA_HOME:-}" ] && [ -f "$HOME/.xzshrc" ]; then
  JH="$(sed -n 's/^export JAVA_HOME=//p' "$HOME/.xzshrc" | tail -n 1 | tr -d '"' | tr -d "'")"
  [ -n "$JH" ] && [ -d "$JH" ] && export JAVA_HOME="$JH"
fi
if [ -z "${JAVA_HOME:-}" ] && [ -f "$HOME/.zshrc" ]; then
  JH="$(sed -n 's/^export JAVA_HOME=//p' "$HOME/.zshrc" | tail -n 1 | tr -d '"' | tr -d "'")"
  [ -n "$JH" ] && [ -d "$JH" ] && export JAVA_HOME="$JH"
fi

echo "[gate] cargo check"
cargo check

echo "[gate] build ffi release"
cargo build -p privchat-sdk-ffi --release

echo "[gate] generate kotlin bindings"
cd "$ROOT/crates/privchat-sdk-ffi"
cargo run --manifest-path uniffi-bindgen/Cargo.toml --release -- \
  generate ../../target/release/libprivchat_sdk_ffi.dylib \
  --language kotlin --out-dir bindings/kotlin --config uniffi.toml

RUN_IOS_REPRO_VALUE="${RUN_IOS_REPRO:-1}"
if [ "$RUN_IOS_REPRO_VALUE" = "1" ]; then
  echo "[gate] run ios auto repro"
  cd "$ROOT/../privchat-sdk-kotlin"
  ./scripts/ios-auto-repro.sh
fi

echo "[gate] done"
