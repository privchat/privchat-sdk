#!/usr/bin/env bash
# Usage: PRIVCHAT_FAULT_DB_URL=... PRIVCHAT_FAULT_REDIS_URL=... bash ... SERVER_BIN ACCOUNTS_BIN
# These resources must be disposable and unused by other jobs.
set -euo pipefail
server_bin="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
accounts_bin="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
script_dir="$(cd "$(dirname "$0")" && pwd)"
: "${PRIVCHAT_FAULT_DB_URL:?Provide a fresh local privchat_fault_* database}"
: "${PRIVCHAT_FAULT_REDIS_URL:?Provide a dedicated local Redis instance}"
[[ "$PRIVCHAT_FAULT_DB_URL" =~ ^postgres(ql)?://([^/@]+@)?(127\.0\.0\.1|localhost)(:[0-9]+)?/privchat_fault_[A-Za-z0-9_]+$ ]]
[[ "$PRIVCHAT_FAULT_REDIS_URL" =~ ^redis://(127\.0\.0\.1|localhost):[0-9]+(/0)?$ ]]
export DATABASE_URL="$PRIVCHAT_FAULT_DB_URL" REDIS_URL="$PRIVCHAT_FAULT_REDIS_URL"
[[ "$(psql -XAt -v ON_ERROR_STOP=1 "$DATABASE_URL" -c "SELECT count(*) FROM pg_tables WHERE schemaname='public'")" = 0 ]]
[[ "$(redis-cli -u "$REDIS_URL" DBSIZE)" = 0 ]]
export SERVICE_MASTER_KEY
SERVICE_MASTER_KEY=$(openssl rand -hex 32)
umask 077
root=$(mktemp -d "${TMPDIR:-/tmp}/privchat-restart-gate.XXXXXX")
echo "Restart gate evidence: $root"
server_pid= proxy_pid= accounts_pid= prometheus_pid= alertmanager_pid= webhook_pid=
stop() {
    if [[ -n "$1" ]]; then kill "$1" 2>/dev/null || true; wait "$1" 2>/dev/null || true; fi
}
cleanup() {
    if [[ -n "$accounts_pid" ]]; then
        kill -TERM -- "-$accounts_pid" 2>/dev/null || true
        wait "$accounts_pid" 2>/dev/null || true
    fi
    stop "$prometheus_pid"; stop "$alertmanager_pid"; stop "$webhook_pid"
    stop "$proxy_pid"; stop "$server_pid"
    # The fixture contains a disposable login. Do not retain it with logs.
    for secret in "$root/fixture" "$root/fixture.tmp"; do
        if [[ -f "$secret" ]]; then unlink "$secret"; fi
    done
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
unset PRIVCHAT_RESTART_FAULT_MODE
cd "$root"
export PRIVCHAT_RESTART_FAULT_DIR="$root"
read -r PRIVCHAT_TCP_PORT ws admin file PRIVCHAT_FAULT_PROXY_PORT < <(python3 - <<'PY'
import socket
sockets = [socket.socket() for _ in range(5)]
for sock in sockets:
    sock.bind(("127.0.0.1", 0))
print(*(sock.getsockname()[1] for sock in sockets))
PY
)
export PRIVCHAT_TCP_PORT PRIVCHAT_FAULT_PROXY_PORT PRIVCHAT_TEST_TRANSPORT=tcp PRIVCHAT_PHASES=phase1,phase2
"$server_bin" generate-config "$root/base.toml"
awk -v tcp="$PRIVCHAT_TCP_PORT" -v ws="$ws" -v admin="$admin" -v file="$file" -v redis="$REDIS_URL" '
    /^\[server_event\]/ { skip=1; next }
    /^\[/ { skip=0 }
    skip { next }
    { gsub(/0\.0\.0\.0/, "127.0.0.1") }
    /^port = 9001$/ { $0="port = " tcp }
    /^port = 9080$/ { $0="port = " ws }
    /^port = 9090$/ { print "host = \"127.0.0.1\""; $0="port = " admin }
    /^url = "redis:/ { $0="url = \"" redis "\"" }
    /^server_port = 9083$/ { $0="server_port = " file }
    /server_api_base_url/ { sub(/:9083/, ":" file) }
    { print }
' base.toml >config.toml
PRIVCHAT_SPKI_PINS=$(openssl x509 -in certs/server.crt -pubkey -noout | openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | openssl base64 -A)
export PRIVCHAT_SPKI_PINS
"$server_bin" --config-file "$root/config.toml" migrate >migrate.log 2>&1
wait_for() {
    local seconds=$1
    shift
    local end=$((SECONDS + seconds))
    until "$@"; do
        if (( SECONDS >= end )); then echo "Timeout: $*" >&2; return 1; fi
        sleep 0.25
    done
}
start() {
    "$server_bin" --config-file "$root/config.toml" >>server-runtime.log 2>&1 &
    server_pid=$!
    wait_for 90 curl --noproxy 127.0.0.1 -fsS --max-time 2 -o /dev/null \
        -H "X-Service-Key: $SERVICE_MASTER_KEY" "http://127.0.0.1:$admin/api/service/system/ready" 2>/dev/null
}
start
if [[ "${PRIVCHAT_GATE_MODE:-restart}" == dead-recovery || "${PRIVCHAT_GATE_MODE:-restart}" == dead-recovery-wire ]]; then
    source "$script_dir/dead-recovery-gate.inc.sh"
    run_dead_recovery_gate
    exit 0
fi
python3 "$script_dir/restart-fault-proxy.py" "$root" "$PRIVCHAT_FAULT_PROXY_PORT" "$PRIVCHAT_TCP_PORT" >proxy.log 2>&1 &
proxy_pid=$!
wait_for 10 test -f proxy-ready
mkdir accounts-tmp
TMPDIR="$root/accounts-tmp" python3 -c 'import os, sys; os.setsid(); os.execv(sys.argv[1], [sys.argv[1]])' \
    "$accounts_bin" >accounts.log 2>&1 &
accounts_pid=$!
wait_for 180 test -s local-row
tag=$(jq -er .tag fixture)
[[ "$tag" =~ ^restart-ack-[0-9]+-[0-9]+$ ]]
lookup() {
    psql -XAt -v ON_ERROR_STOP=1 -v "tag=$tag" "$DATABASE_URL" <<'SQL'
SELECT coalesce(min(server_msg_id)::text, '') || ':' ||
       coalesce(min(pts)::text, '') || ':' || count(*)
FROM privchat_commit_log WHERE content->>'content' = :'tag';
SQL
}
observe_commit() {
    match=$(lookup) || return 1
    [[ "$match" =~ ^[0-9]+:[0-9]+:1$ ]] && [[ -f dropped-downstream ]]
}
wait_for 60 observe_commit
IFS=: read -r expected pts _ <<<"$match"
printf '%s' "$pts" >committed-pts
touch commit-confirmed
wait_for 45 test -f restart-server
kill -KILL "$server_pid"
wait "$server_pid" 2>/dev/null || [[ "$?" = 137 ]]
server_pid=
start
printf '%s' "$expected" >server-restarted.tmp
mv server-restarted.tmp server-restarted
wait_for 180 test -s passed
wait "$accounts_pid"
accounts_pid=
[[ "$(cat passed)" = "$expected" ]]
[[ "$(lookup)" = "$expected:$pts:1" ]]
grep -F '[PASS] real ACK loss, sender death, server restart and read reconciliation' accounts.log
