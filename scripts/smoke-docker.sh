#!/usr/bin/env bash
# End-to-end smoke test of the real `ferrobotd` binary with the Docker backend
# and the mock driver (no API key needed). Requires Docker/Podman and the
# runner image: docker build -f runner/Dockerfile -t ferrobot-runner:dev .
#
# Checks: two sandboxed agents talk through the mailbox, the UI API answers,
# and runners reconnect after a daemon restart. Cleans up after itself.
set -euo pipefail
cd "$(dirname "$0")/.."

API=${FERROBOT_API:-http://127.0.0.1:7878}
DATA=$(mktemp -d)
BIN=target/debug/ferrobotd
cargo build -q -p ferrobot-daemon

start() {
  "$BIN" --data-dir "$DATA" --backend docker --driver mock \
    --agent-listen 0.0.0.0:7879 >>"$DATA/daemon.log" 2>&1 &
  PID=$!
  for _ in $(seq 50); do curl -sf "$API/v1/health" >/dev/null && return; sleep 0.2; done
  echo "daemon did not start"; cat "$DATA/daemon.log"; exit 1
}
stop() { kill -TERM "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; }
cleanup() {
  for id in ${ALICE:-} ${BOB:-}; do curl -s -o /dev/null -X DELETE "$API/v1/agents/$id?purge=true" || true; done
  stop; rm -rf "$DATA"
}
trap cleanup EXIT

json() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
post() { curl -sf -X POST "$API$1" -H 'content-type: application/json' -d "$2"; }
wait_for() { # wait_for <description> <command>
  for _ in $(seq 60); do if eval "$2" >/dev/null 2>&1; then echo "ok: $1"; return; fi; sleep 0.5; done
  echo "FAILED: $1"; tail -20 "$DATA/daemon.log"; exit 1
}
delivered() { # delivered <agent-id> <body>
  curl -sf "$API/v1/agents/$1/messages" | json "any(m['body']=='$2' and m['status']=='delivered' for m in d)" | grep -q True
}

start
curl -sf "$API/v1/health"; echo
ALICE=$(post /v1/agents '{"name":"smoke-alice"}' | json "d['id']")
BOB=$(post /v1/agents '{"name":"smoke-bob"}' | json "d['id']")
wait_for "both runners connected" \
  "curl -sf $API/v1/agents | json \"all(a['connected'] for a in d)\" | grep -q True"

post "/v1/agents/$ALICE/messages" '{"body":"@smoke-bob hello"}' >/dev/null
wait_for "alice -> bob delivered" "delivered $BOB hello"
wait_for "bob's auto-reply reached alice" "delivered $ALICE 're: hello'"

echo "restarting daemon"
stop; start
wait_for "runners reconnected" \
  "curl -sf $API/v1/agents | json \"all(a['connected'] for a in d)\" | grep -q True"
post "/v1/agents/$ALICE/messages" '{"body":"@smoke-bob again"}' >/dev/null
wait_for "mailbox works after restart" "delivered $BOB again"

post "/v1/agents/$BOB/exec" '{"cmd":["sh","-c","id -un"]}' | json "d['stdout'].strip()"
echo "smoke test passed"
