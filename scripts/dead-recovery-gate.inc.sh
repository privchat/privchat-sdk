#!/usr/bin/env bash
# Sourced only by restart-fault-gate.sh after its isolated server is ready.
run_dead_recovery_gate() {
    if [[ "$PRIVCHAT_GATE_MODE" == dead-recovery ]]; then
        : "${PRIVCHAT_PROMETHEUS_BIN:?Set the Prometheus binary path}"
        : "${PRIVCHAT_ALERTMANAGER_BIN:?Set the Alertmanager binary path}"
        : "${PRIVCHAT_DISPATCH_RULES:?Set the repository alert rule file path}"
        [[ -x "$PRIVCHAT_PROMETHEUS_BIN" && -x "$PRIVCHAT_ALERTMANAGER_BIN" && -r "$PRIVCHAT_DISPATCH_RULES" ]]

        local prom alert webhook
        read -r prom alert webhook < <(python3 - <<'PY'
import socket
sockets = [socket.socket() for _ in range(3)]
for sock in sockets:
    sock.bind(("127.0.0.1", 0))
print(*(sock.getsockname()[1] for sock in sockets))
PY
    )
        cp "$PRIVCHAT_DISPATCH_RULES" dispatch.rules.yml
        cat >alertmanager.yml <<YAML
route:
  receiver: acceptance-webhook
  group_wait: 0s
  group_interval: 1s
  repeat_interval: 1h
receivers:
  - name: acceptance-webhook
    webhook_configs:
      - url: http://127.0.0.1:$webhook/alert
YAML
        cat >prometheus.yml <<YAML
global:
  scrape_interval: 1s
  scrape_timeout: 1s
  evaluation_interval: 1s
rule_files:
  - dispatch.rules.yml
alerting:
  alertmanagers:
    - static_configs:
        - targets: ["127.0.0.1:$alert"]
scrape_configs:
  - job_name: privchat-acceptance
    static_configs:
      - targets: ["127.0.0.1:$file"]
YAML
        python3 "$script_dir/dead-recovery-webhook.py" "$webhook" "$root/alert-delivered.json" >webhook.log 2>&1 &
        webhook_pid=$!
        wait_for 10 test -f alert-delivered.ready
        "$PRIVCHAT_ALERTMANAGER_BIN" --config.file=alertmanager.yml \
            --storage.path="$root/alert-data" --web.listen-address="127.0.0.1:$alert" \
            --cluster.listen-address= >alertmanager.log 2>&1 &
        alertmanager_pid=$!
        wait_for 20 curl --noproxy 127.0.0.1 -fsS --max-time 2 -o /dev/null "http://127.0.0.1:$alert/-/ready"
        "$PRIVCHAT_PROMETHEUS_BIN" --config.file=prometheus.yml \
            --storage.tsdb.path="$root/prom-data" --web.listen-address="127.0.0.1:$prom" >prometheus.log 2>&1 &
        prometheus_pid=$!
        wait_for 20 curl --noproxy 127.0.0.1 -fsS --max-time 2 -o /dev/null "http://127.0.0.1:$prom/-/ready"

        metrics_baseline() {
            curl --noproxy 127.0.0.1 -fsSG --max-time 2 "http://127.0.0.1:$prom/api/v1/query" \
                --data-urlencode 'query=count_over_time(privchat_timeline_dispatch_dead_total{job="privchat-acceptance",reason="lease_expired"}[30s]) >= 2' \
                | jq -e '.status == "success" and (.data.result | length) == 1' >/dev/null &&
            curl --noproxy 127.0.0.1 -fsSG --max-time 2 "http://127.0.0.1:$prom/api/v1/query" \
                --data-urlencode 'query=privchat_timeline_dispatch_dead_total{job="privchat-acceptance",reason="lease_expired"} == 0' \
                | jq -e '.status == "success" and (.data.result | length) == 1' >/dev/null
        }
        wait_for 30 metrics_baseline
    fi

    unset PRIVCHAT_RESTART_FAULT_DIR
    export PRIVCHAT_DEAD_RECOVERY_DIR="$root" PRIVCHAT_ADMIN_API_PORT="$admin" PRIVCHAT_INBOUND_LOG=1
    mkdir accounts-tmp
    TMPDIR="$root/accounts-tmp" python3 -c 'import os,sys; os.setsid(); os.execv(sys.argv[1], [sys.argv[1]])' \
        "$accounts_bin" >accounts.log 2>&1 &
    accounts_pid=$!
    wait_for 120 test -s fixture
    local channel alice bob message body
    channel=$(jq -er .channel fixture)
    alice=$(jq -er .alice fixture)
    bob=$(jq -er .bob fixture)
    message=$(jq -er .message fixture)
    body=$(jq -er .body fixture)
    [[ "$channel" =~ ^[0-9]+$ && "$alice" =~ ^[0-9]+$ && "$bob" =~ ^[0-9]+$ && "$message" =~ ^[0-9]+$ ]]

    # Create a committed timeline event with only a final expired lease.
    # The real worker, not the harness, must transition it to DEAD.
    psql -Xq -v ON_ERROR_STOP=1 "$DATABASE_URL" -v "channel=$channel" -v "alice=$alice" \
        -v "bob=$bob" -v "message=$message" -v "body=$body" <<'SQL'
BEGIN;
WITH watermark AS (
    INSERT INTO privchat_channel_pts (channel_id, current_pts, created_at, updated_at)
        VALUES (:'channel', 1, now_millis(), now_millis())
    ON CONFLICT (channel_id) DO UPDATE
        SET current_pts = privchat_channel_pts.current_pts + 1, updated_at = now_millis()
    RETURNING current_pts
)
INSERT INTO privchat_commit_log
    (id, pts, server_msg_id, channel_id, channel_type, message_type, content, server_timestamp, sender_id, created_at)
SELECT :'message', current_pts, :'message', :'channel', 1, 'text', jsonb_build_object('content', :'body'::text),
    now_millis(), :'alice', now_millis() FROM watermark;
INSERT INTO privchat_message_dispatch_outbox
    (event_id, channel_id, channel_type, pts, sender_id, event_kind, membership_version, status, created_at)
SELECT c.id,c.channel_id,0,c.pts,c.sender_id,1,ch.membership_version,0,now_millis()
FROM privchat_commit_log c JOIN privchat_channels ch ON ch.channel_id=c.channel_id WHERE c.id=:'message';
INSERT INTO privchat_message_dispatch_recipient (event_id,user_id,state,attempts,lease_until)
    VALUES (:'message', :'bob', 0, 8, 0);
COMMIT;
SQL

    expiry_persisted() {
        [[ "$(psql -XAt -v ON_ERROR_STOP=1 "$DATABASE_URL" -c "SELECT state || ':' || attempts FROM privchat_message_dispatch_recipient WHERE event_id=$message AND user_id=$bob")" = "3:8" ]]
    }
    wait_for 20 expiry_persisted
    touch dead-ready
    wait_for 40 test -f wire-passed
    wait "$accounts_pid"
    accounts_pid=
    grep -F '[PASS] one bodyless wire hint, no replay, one authorized local projection' accounts.log
    if [[ "$PRIVCHAT_GATE_MODE" == dead-recovery ]]; then
        wait_for 40 test -s alert-delivered.json
        jq -e '.receiver == "acceptance-webhook" and any(.alerts[]; .status == "firing" and .labels.alertname == "PrivChatTimelineDeliveryDead" and .labels.job == "privchat-acceptance")' alert-delivered.json >/dev/null
        echo '[PASS] real Prometheus rule fired and Alertmanager delivered the webhook'
    fi
    [[ "$(psql -XAt -v ON_ERROR_STOP=1 "$DATABASE_URL" -c "SELECT count(*) FROM privchat_timeline_recovery_audit WHERE event_id=$message AND user_id=$bob AND notified_sessions=1")" = 1 ]]
    expiry_persisted
    [[ "$(redis-cli -u "$REDIS_URL" exists "offline:$bob:messages")" = 0 ]]
}
