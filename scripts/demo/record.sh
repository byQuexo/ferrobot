#!/usr/bin/env bash
# Records docs/assets/demo.{mp4,gif} with a real model: a throwaway ferrobotd (Docker, spare ports, file
# secret store, temp data dir), the Vite UI and record.mjs driving Chromium. The provider's API key comes
# on stdin and only ever reaches that daemon's secret store, which is deleted on exit.
#   <print the API key> | scripts/demo/record.sh
# Env: DEMO_PROVIDER_KIND (openrouter), DEMO_MODEL (tencent/hy4-flash), DEMO_BASE_URL, DEMO_PORT (17990),
# DEMO_AGENT_PORT (17991), DEMO_UI_PORT (1421), DEMO_KEEP=1 keeps the data dir and frames.
set -euo pipefail
cd "$(dirname "$0")/../.."

PORT=${DEMO_PORT:-17990}
AGENT_PORT=${DEMO_AGENT_PORT:-17991}
UI_PORT=${DEMO_UI_PORT:-1421}
API=http://127.0.0.1:$PORT
DATA=$(mktemp -d)
BIN=target/debug/ferrobotd
OUT=docs/assets
[ -x "$BIN" ] || cargo build -q -p ferrobot-daemon
[ -t 0 ] && { echo "pipe the provider's API key on stdin"; exit 1; }
for p in "$PORT" "$AGENT_PORT" "$UI_PORT"; do
  if lsof -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1; then echo "port $p is in use"; exit 1; fi
done

acurl() { curl -sf -H "Authorization: Bearer $(cat "$DATA/api-token")" "$@"; }
cleanup() {
  if [ -f "$DATA/api-token" ]; then
    for id in $(acurl "$API/v1/agents" | python3 -c 'import json,sys; print(" ".join(a["id"] for a in json.load(sys.stdin)))' || true); do
      acurl -o /dev/null -X DELETE "$API/v1/agents/$id" || true
    done
  fi
  kill -TERM ${UI_PID:-} ${PID:-} 2>/dev/null || true
  wait 2>/dev/null || true
  if [ "${DEMO_KEEP:-}" = 1 ]; then echo "kept $DATA"; else rm -rf "$DATA"; fi
}
trap cleanup EXIT

"$BIN" --data-dir "$DATA" --secret-store file --backend docker --driver claude --idle-timeout 0 \
  --timezone UTC --listen "127.0.0.1:$PORT" --agent-listen "0.0.0.0:$AGENT_PORT" \
  </dev/null >>"$DATA/daemon.log" 2>&1 &
PID=$!
(cd apps/desktop && exec node_modules/.bin/vite --port "$UI_PORT" --strictPort) </dev/null >>"$DATA/vite.log" 2>&1 &
UI_PID=$!
for _ in $(seq 100); do curl -sf "$API/v1/health" >/dev/null && curl -sf "http://localhost:$UI_PORT" >/dev/null && break; sleep 0.2; done

mkdir -p "$DATA/frames"
DEMO_API=$API DEMO_UI=http://localhost:$UI_PORT DEMO_TOKEN_FILE=$DATA/api-token DEMO_FRAMES=$DATA/frames \
  node scripts/demo/record.mjs

acurl "$API/v1/usage" | python3 -c 'import json,sys; t=json.load(sys.stdin)["totals"]; print("model calls:", t["calls"], "cost (USD):", t["cost_usd"])'

# Variable frame durations (waits sped up) -> constant 30 fps.
ffmpeg -loglevel error -y -f concat -safe 0 -i "$DATA/frames/frames.txt" \
  -vf "fps=30,scale=1280:-2:flags=lanczos" -c:v libx264 -preset slow -crf 24 -pix_fmt yuv420p \
  -movflags +faststart "$OUT/demo.mp4"
ffmpeg -loglevel error -y -i "$OUT/demo.mp4" -vf \
  "fps=12,scale=960:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle" \
  "$OUT/demo.gif"
ls -l "$OUT/demo.mp4" "$OUT/demo.gif"
