#!/usr/bin/env bash
set -euo pipefail

ROOT="/Users/zoujiaqing/projects/privchat"
SERVER="$ROOT/privchat-server"
SDK="$ROOT/privchat-sdk"

echo "[sync-regression] server cargo test"
(cd "$SERVER" && cargo test -- --nocapture)

if [[ -n "${DATABASE_URL:-${PRIVCHAT_TEST_DATABASE_URL:-}}" ]]; then
  echo "[sync-regression] server entity_sync_version_db_test"
  (cd "$SERVER" && cargo test --test entity_sync_version_db_test -- --nocapture)
else
  echo "[sync-regression] skip DB integration test (DATABASE_URL/PRIVCHAT_TEST_DATABASE_URL not set)"
fi

echo "[sync-regression] sdk lib tests"
(cd "$SDK" && cargo test -p privchat-sdk --lib -- --nocapture)

echo "[sync-regression] accounts smoke"
(cd "$SDK" && cargo run --quiet --example accounts)
