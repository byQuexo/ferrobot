#!/usr/bin/env bash
# Live smoke test: real models, real Docker sandboxes, a throwaway daemon.
#
#   scripts/smoke-live.sh      # in your own terminal
#
# It asks for the Anthropic API key without echoing it, so the key stays out
# of the terminal and the shell history (Enter skips the Claude part); an
# ANTHROPIC_API_KEY already in the environment is used as is. The key goes the
# way a key from Settings goes: into an Anthropic provider, stored by the
# daemon in the OS keychain (macOS Keychain, Windows Credential Manager,
# Secret Service). The daemon itself gets no ANTHROPIC_API_KEY. The provider
# and its keychain entry are deleted at the end.
#
# 1. A Claude agent (on that provider) runs one
#    tool-using turn: it writes a file with its shell tool and reports it.
# 2. An agent on local Ollama (OpenAI-compatible driver) does the same, if
#    Ollama runs and has OLLAMA_MODEL (default qwen2.5:3b; `ollama pull` it).
# 3. The real key must not appear in the container env, its files (root
#    filesystem and home volume), container logs, the daemon log or the data
#    dir. The key is never printed.
#
# Needs Docker; rebuilds the runner image first (SKIP_BUILD=1 skips that).
# Uses ports 17878/17879, so a running ferrobot is left alone. Cleans up
# after itself.
set -euo pipefail
cd "$(dirname "$0")/.."

API=http://127.0.0.1:17878
DATA=$(mktemp -d)
BIN=target/debug/ferrobotd
OLLAMA_MODEL=${OLLAMA_MODEL:-qwen2.5:3b}
TOKEN=smoke-live-operator-token-0123456789
FAILED=0
KEY=${ANTHROPIC_API_KEY:-}
# A shell variable from here on: no child process (docker, curl, cargo) inherits it.
unset ANTHROPIC_API_KEY
if [ -z "$KEY" ] && [ -t 0 ]; then
  read -rsp "Anthropic API key (hidden; Enter skips Claude): " KEY
  echo
fi
cargo build -q -p ferrobot-daemon
[ -n "${SKIP_BUILD:-}" ] || docker build -q -f runner/Dockerfile -t ferrobot-runner:dev . >/dev/null

# No key in the daemon's environment: Claude works only through the keychain.
env -u ANTHROPIC_API_KEY "$BIN" --data-dir "$DATA" --backend docker --driver claude --idle-timeout 0 \
  --secret-store keyring --listen 127.0.0.1:17878 --agent-listen 0.0.0.0:17879 \
  --api-token "$TOKEN" >>"$DATA/daemon.log" 2>&1 &
PID=$!
stop_daemon() {
  for id in ${CLAUDE:-} ${OLLAMA:-}; do acurl -s -o /dev/null -X DELETE "$API/v1/agents/$id" || true; done
  # Deleting the provider deletes its keychain entry.
  [ -z "${ANTHROPIC:-}" ] || acurl -s -o /dev/null -X DELETE "$API/v1/providers/$ANTHROPIC" || true
  CLAUDE= OLLAMA= ANTHROPIC=
  kill -TERM "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true
}
# Whether the keychain holds a provider's key (macOS; elsewhere unknown).
in_keychain() {
  command -v security >/dev/null || return 2
  # `security` exits 44 for a missing item: normalise to 1.
  security find-generic-password -s ferrobot -a "provider:$1" >/dev/null 2>&1 || return 1
}
cleanup() {
  stop_daemon
  rm -rf "$DATA"
  docker ps -a --format '{{.Names}}' | grep '^ferrobot-live-' && echo "WARNING: containers left" || true
}
trap cleanup EXIT
for _ in $(seq 50); do curl -sf "$API/v1/health" >/dev/null && break; sleep 0.2; done

acurl() { curl -H "Authorization: Bearer $TOKEN" "$@"; }
json() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
get() { acurl -sf "$API$1"; }
post() { acurl -sf -X POST "$API$1" -H 'content-type: application/json' -d "$2"; }
ok() { echo "ok: $1"; }
fail() { echo "FAILED: $1"; FAILED=1; }

# One tool-using turn: the agent writes a nonce to a file and reports it.
turn() { # turn <agent-id> <label>
  local id=$1 label=$2 nonce="smoke-$RANDOM$RANDOM"
  post "/v1/agents/$id/messages" "{\"body\":\"Use your shell tool to run: echo $nonce > ~/workspace/proof.txt -- then reply with the file's content.\"}" >/dev/null
  for _ in $(seq 180); do
    get "/v1/agents/$id/events" | json "any(e['kind']=='turn_done' for e in d)" | grep -q True && break
    sleep 1
  done
  local done_ev
  done_ev=$(get "/v1/agents/$id/events" | json "json.dumps([e['payload'] for e in d if e['kind']=='turn_done'][-1:])")
  echo "  turn: $done_ev" | cut -c1-300
  # A turn that hangs, fails or answers without the file's content is a failure.
  echo "$done_ev" | json "len(d) == 1 and d[0]['outcome'] == 'completed' and '$nonce' in (d[0]['result'] or '')" | grep -qx True \
    && ok "$label finished the turn and reported the content" || fail "$label: no completed turn reporting $nonce"
  get "/v1/agents/$id/events" | json "any(e['kind']=='tool_use' for e in d)" | grep -q True \
    && ok "$label used a tool" || fail "$label used no tool"
  post "/v1/agents/$id/exec" '{"cmd":["cat","/home/node/workspace/proof.txt"]}' | json "d['stdout'].strip()" | grep -qx "$nonce" \
    && ok "$label wrote the file in its sandbox" || fail "$label did not write the file"
}

# The real key must be nowhere the agent or a log could see it. grep -c reads
# the whole stream (no early exit, no SIGPIPE under pipefail); a source that
# cannot be read counts as a failure, not as clean. The pattern comes from a
# pipe, not argv, so the key never shows up in `ps`.
leak_check() { # leak_check <what> <command...>
  local what=$1 count; shift
  if ! count=$("$@" 2>/dev/null | grep -acFf <(printf '%s\n' "$KEY"); exit "${PIPESTATUS[0]}"); then
    fail "could not read $what"
  elif [ "$count" = 0 ]; then ok "key not in $what"
  else fail "key in $what"
  fi
}
no_key() { # no_key <container> <label>
  [ -n "$KEY" ] || return 0
  leak_check "$2: container config and env" docker inspect "$1"
  leak_check "$2: runtime env" docker exec "$1" env
  leak_check "$2: root filesystem" docker export "$1"
  leak_check "$2: home volume" docker cp "$1:/home/node" -
  leak_check "$2: container logs" docker logs "$1"
}

if [ -n "$KEY" ]; then
  # The key reaches the daemon on stdin, never in an argument list (`ps`).
  ANTHROPIC=$(printf '%s' "$KEY" \
    | python3 -c 'import json,sys; print(json.dumps({"name": "live-anthropic", "kind": "anthropic", "api_key": sys.stdin.read()}))' \
    | acurl -sf -X POST "$API/v1/providers" -H 'content-type: application/json' --data-binary @- | json "d['id']")
  get "/v1/providers" | json "[p for p in d if p['id']=='$ANTHROPIC'][0]['has_key']" | grep -qx True \
    && ok "provider reports a stored key" || fail "provider has no key"
  if in_keychain "$ANTHROPIC"; then ok "key is in the OS keychain"
  elif [ $? = 2 ]; then echo "  (keychain entry not checked on this OS)"
  else fail "key not found in the OS keychain"
  fi
  CLAUDE=$(post /v1/agents "{\"name\":\"live-claude\",\"provider_id\":\"$ANTHROPIC\"}" | json "d['id']")
  turn "$CLAUDE" claude
  no_key ferrobot-live-claude claude
else
  echo "skipped: Claude (no Anthropic API key given)"
fi

if curl -sf -m 3 http://127.0.0.1:11434/api/tags | json "any(m['name']=='$OLLAMA_MODEL' for m in d['models'])" | grep -q True; then
  PROVIDER=$(post /v1/providers "{\"name\":\"ollama\",\"kind\":\"ollama\",\"default_model\":\"$OLLAMA_MODEL\"}" | json "d['id']")
  OLLAMA=$(post /v1/agents "{\"name\":\"live-ollama\",\"provider_id\":\"$PROVIDER\"}" | json "d['id']")
  turn "$OLLAMA" ollama
  no_key ferrobot-live-ollama ollama
else
  echo "skipped: Ollama (not running on :11434, or $OLLAMA_MODEL not pulled)"
fi

# The daemon stops first, so nothing writes to the data dir while it is read.
GONE=${ANTHROPIC:-}
stop_daemon
if [ -n "$GONE" ]; then
  if in_keychain "$GONE"; then fail "keychain entry left after deleting the provider"
  elif [ $? = 1 ]; then ok "deleting the provider removed its keychain entry"
  fi
fi
if [ -n "$KEY" ]; then
  leak_check "the daemon log" cat "$DATA/daemon.log"
  leak_check "the data dir (database, secrets, logs)" tar -C "$DATA" -cf - .
fi
[ "$FAILED" = 0 ] && echo "live smoke test passed" || { echo "live smoke test FAILED"; tail -40 "$DATA/daemon.log"; exit 1; }
