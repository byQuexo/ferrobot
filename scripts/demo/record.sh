#!/usr/bin/env bash
# Records one demo scene (scripts/demo/record.mjs) from the Vite UI in Chromium.
#   DEMO_API=http://127.0.0.1:17990 DEMO_TOKEN_FILE=<data dir>/api-token scripts/demo/record.sh hero
# uses that daemon and its configured providers (real models). Without DEMO_API it starts a throwaway
# daemon with the mock driver on 17992/17993 for a keyless dry run. Scenes: hero, keycard, routine,
# memory, usage, shots. DEMO_KEEP=1 keeps the frames dir.
set -euo pipefail
cd "$(dirname "$0")/../.."
SCENE=${1:?"scene: hero, keycard, routine, memory, usage or shots"}
WORK=$(mktemp -d)
UI_PORT=1420
lsof -iTCP:1420 -sTCP:LISTEN >/dev/null 2>&1 && UI_PORT=1421

cleanup() {
  if [ -n "${PID:-}" ] && [ -f "$WORK/data/api-token" ]; then
    auth="Authorization: Bearer $(cat "$WORK/data/api-token")"
    for id in $(curl -sf -H "$auth" "$DEMO_API/v1/agents" | python3 -c 'import json,sys; print(" ".join(a["id"] for a in json.load(sys.stdin)))' || true); do
      curl -sf -o /dev/null -H "$auth" -X DELETE "$DEMO_API/v1/agents/$id" || true
    done
  fi
  kill -TERM ${UI_PID:-} ${PID:-} 2>/dev/null || true
  wait 2>/dev/null || true
  if [ "${DEMO_KEEP:-}" = 1 ]; then echo "kept $WORK"; else rm -rf "$WORK"; fi
}
trap cleanup EXIT

if [ -z "${DEMO_API:-}" ]; then
  [ -x target/debug/ferrobotd ] || cargo build -q -p ferrobot-daemon
  for p in 17992 17993; do
    if lsof -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1; then echo "port $p is in use"; exit 1; fi
  done
  mkdir -p "$WORK/data"
  target/debug/ferrobotd --data-dir "$WORK/data" --secret-store file --backend docker --driver mock \
    --idle-timeout 0 --timezone UTC --listen 127.0.0.1:17992 --agent-listen 0.0.0.0:17993 \
    </dev/null >>"$WORK/daemon.log" 2>&1 &
  PID=$!
  export DEMO_API=http://127.0.0.1:17992 DEMO_TOKEN_FILE=$WORK/data/api-token
fi
(cd apps/desktop && exec node_modules/.bin/vite --port "$UI_PORT" --strictPort) </dev/null >>"$WORK/vite.log" 2>&1 &
UI_PID=$!
for _ in $(seq 100); do
  curl -sf "$DEMO_API/v1/health" >/dev/null && curl -sf "http://localhost:$UI_PORT" >/dev/null && break
  sleep 0.2
done

DEMO_UI=http://localhost:$UI_PORT DEMO_FRAMES=$WORK/frames node scripts/demo/record.mjs "$SCENE"
