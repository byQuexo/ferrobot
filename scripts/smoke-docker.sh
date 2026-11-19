#!/usr/bin/env bash
# End-to-end smoke test of the real `ferrobotd` binary with the Docker backend
# and the mock driver (no API key needed). Requires Docker/Podman and the
# runner image: docker build -f runner/Dockerfile -t ferrobot-runner:dev .
#
# Checks: two sandboxed agents talk through the mailbox; memory, routines and
# profiles over the API; idle auto-sleep and wake-on-message; snapshot and
# restore of an agent's computer; persistence across a daemon restart; the
# operator token; renaming; deleting an agent removes its disk.
# Cleans up after itself.
set -euo pipefail
cd "$(dirname "$0")/.."

API=${FERROBOT_API:-http://127.0.0.1:7878}
DATA=$(mktemp -d)
BIN=target/debug/ferrobotd
IDLE=${FERROBOT_SMOKE_IDLE:-4}
cargo build -q -p ferrobot-daemon

start() {
  "$BIN" --data-dir "$DATA" --backend docker --driver mock --idle-timeout "$IDLE" \
    --timezone UTC --agent-listen 0.0.0.0:7879 >>"$DATA/daemon.log" 2>&1 &
  PID=$!
  for _ in $(seq 50); do curl -sf "$API/v1/health" >/dev/null && return; sleep 0.2; done
  echo "daemon did not start"; cat "$DATA/daemon.log"; exit 1
}
stop() { kill -TERM "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; }
cleanup() {
  for id in ${ALICE:-} ${BOB:-}; do acurl -s -o /dev/null -X DELETE "$API/v1/agents/$id" || true; done
  stop; rm -rf "$DATA"
}
trap cleanup EXIT

# Every /v1 call except health needs the operator token (read after start).
acurl() { curl -H "Authorization: Bearer ${TOKEN:-}" "$@"; }
json() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
get() { acurl -sf "$API$1"; }
post() { acurl -sf -X POST "$API$1" -H 'content-type: application/json' -d "$2"; }
wait_for() { # wait_for <description> <command>
  for _ in $(seq 80); do if eval "$2" >/dev/null 2>&1; then echo "ok: $1"; return; fi; sleep 0.5; done
  echo "FAILED: $1"; tail -30 "$DATA/daemon.log"; exit 1
}
check() { # check <description> <command>
  if eval "$2" >/dev/null 2>&1; then echo "ok: $1"; else echo "FAILED: $1"; tail -30 "$DATA/daemon.log"; exit 1; fi
}
delivered() { # delivered <agent-id> <body>
  get "/v1/agents/$1/messages" | json "any(m['body']=='$2' and m['status']=='delivered' for m in d)" | grep -q True
}
field() { get "/v1/agents/$1" | json "d['$2']"; }
connected() { get /v1/agents | json "all(a['connected'] for a in d)" | grep -q True; }

start
TOKEN=$(cat "$DATA/api-token")
check "token file is private (0600)" "stat -c %a '$DATA/api-token' | grep -qx 600"
check "API rejects requests without the token" \
  "curl -s -o /dev/null -w '%{http_code}' $API/v1/agents | grep -qx 401"
get /v1/health; echo
ALICE=$(post /v1/agents '{"name":"smoke-alice","description":"Smoke test lead"}' | json "d['id']")
BOB=$(post /v1/agents '{"name":"smoke-bob"}' | json "d['id']")
wait_for "both runners connected" connected

# agent-to-agent mail with tool calls
post "/v1/agents/$ALICE/messages" '{"body":"@smoke-bob hello"}' >/dev/null
wait_for "alice -> bob delivered" "delivered $BOB hello"
wait_for "bob's auto-reply reached alice" "delivered $ALICE 're: hello'"
check "@mention stored by agent id" "get /v1/agents/$ALICE/messages | json \"d[0]['body']\" | grep -qF '<@$BOB> hello'"
check "agent-to-agent thread listed" "get /v1/conversations | json \"len(d) == 1 and d[0]['messages'] == 2\" | grep -qx True"
check "tool call paired with its result" \
  "get /v1/agents/$ALICE/events | json \"len({e['payload'].get('id') for e in d if e['kind']=='tool_use'} & {e['payload'].get('id') for e in d if e['kind']=='tool_result'})>0\" | grep -q True"

# memory: written by the agent over MCP, read and edited over the API
post "/v1/agents/$BOB/messages" '{"body":"remember color: blue"}' >/dev/null
wait_for "memory written via MCP" "get '/v1/agents/$BOB/memory?q=color' | json \"d[0]['content']\" | grep -qx blue"
acurl -sf -X POST "$API/v1/agents/$BOB/memory" -H 'content-type: application/json' -d '{"key":"team","content":"alice and bob"}'
check "memory written via API" "get '/v1/agents/$BOB/memory' | json 'len(d)' | grep -qx 2"

# profile edits, including a rename (the sandbox keeps its name)
acurl -sf -X PATCH "$API/v1/agents/$BOB" -H 'content-type: application/json' \
  -d '{"name":"smoke-robert","description":"Replies to everyone","instructions":"Be terse."}' >/dev/null
check "profile updated" "field $BOB description | grep -qx 'Replies to everyone'"
check "renamed, same sandbox" "field $BOB name | grep -qx smoke-robert && field $BOB sandbox_name | grep -qx smoke-bob"

# routines: run now reaches the agent as a message from routine:<name>
RID=$(post "/v1/agents/$ALICE/routines" '{"name":"digest","schedule":"0 9 * * 1-5","prompt":"daily digest"}' | json "d['id']")
post "/v1/routines/$RID/run" '' >/dev/null
wait_for "routine prompt delivered" "delivered $ALICE 'daily digest'"
check "routine run recorded" "get /v1/routines/$RID/runs | json \"d[0]['status']\" | grep -qx delivered"

# snapshot + restore of the agent's computer
post "/v1/agents/$BOB/exec" '{"cmd":["sh","-c","echo before > ~/workspace/state.txt"]}' >/dev/null
SID=$(post "/v1/agents/$BOB/snapshots" '{"label":"smoke"}' | json "d['id']")
post "/v1/agents/$BOB/exec" '{"cmd":["sh","-c","echo after > ~/workspace/state.txt"]}' >/dev/null
post "/v1/agents/$BOB/snapshots/$SID/restore" '' >/dev/null
check "file restored from snapshot" \
  "post /v1/agents/$BOB/exec '{\"cmd\":[\"cat\",\"/home/node/workspace/state.txt\"]}' | json \"d['stdout'].strip()\" | grep -qx before"
wait_for "restored runner reconnected" "field $BOB connected | grep -qx True"

# idle auto-sleep, then wake on a new message
wait_for "alice fell asleep after ${IDLE}s idle" "field $ALICE presence | grep -qx sleeping"
check "alice's container is stopped" "get /v1/agents/$ALICE/sandbox | json \"d['state']\" | grep -qx stopped"
post "/v1/agents/$ALICE/messages" '{"body":"wake up"}' >/dev/null
wait_for "message woke alice and was answered" "delivered $ALICE 'wake up'"

acurl -sf -X POST "$API/v1/agents/$BOB/stop"
echo "restarting daemon"
stop; start
check "sleeping agent stays asleep across restart" "field $BOB status | grep -qx stopped"
check "its container is still stopped" "get /v1/agents/$BOB/sandbox | json \"d['state']\" | grep -qx stopped"
check "routine survived restart" "get /v1/agents/$ALICE/routines | json \"d[0]['name']\" | grep -qx digest"
check "memory survived restart" "get /v1/agents/$BOB/memory | json 'len(d)' | grep -qx 2"
check "profile survived restart" "field $BOB name | grep -qx smoke-robert"
post "/v1/agents/$ALICE/messages" '{"body":"@smoke-robert again"}' >/dev/null
wait_for "mail to the new name after restart wakes both agents" "delivered $BOB again"
check "sandbox runs as the unprivileged user" \
  "post /v1/agents/$BOB/exec '{\"cmd\":[\"id\",\"-un\"]}' | json \"d['stdout'].strip()\" | grep -qx node"

acurl -sf -X DELETE "$API/v1/agents/$BOB"
check "deleting an agent removes its disk" "! docker volume inspect ferrobot-smoke-bob-home"
BOB=
echo "smoke test passed"
