#!/usr/bin/env bash
# Seeds a running daemon (mock driver works) with a few demo agents and some
# real activity: agent-to-agent mail, tool calls, memory, routines, a sleeping
# agent. Used for the screenshots in docs/. Usage: scripts/seed-demo.sh [avatar.png]
set -euo pipefail
API=${FERROBOT_API:-http://127.0.0.1:7878}
json() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
post() { curl -sf -X POST "$API$1" -H 'content-type: application/json' -d "$2"; }
agent() { post /v1/agents "$1" | json "d['id']"; }
connected() { curl -sf "$API/v1/agents" | json "all(a['connected'] for a in d)" | grep -q True; }

ADA=$(agent '{"name":"ada","description":"Research lead, plans the week","instructions":"You lead research. Delegate reviews to bob and keep notes in memory."}')
BOB=$(agent '{"name":"bob","description":"Reviews code and answers fast","instructions":"You review pull requests and reply briefly."}')
ATLAS=$(agent '{"name":"atlas","description":"Writes release notes"}')
NOVA=$(agent '{"name":"nova-ops","description":"Watches deploys overnight"}')
if [[ -n "${1:-}" ]]; then
  curl -sf -X PUT "$API/v1/agents/$ADA/avatar" -H 'content-type: image/png' --data-binary "@$1" >/dev/null
fi
for _ in $(seq 60); do connected && break; sleep 0.5; done

post "/v1/agents/$ADA/messages" '{"body":"@bob can you review the parser change in PR 42?"}' >/dev/null
sleep 2
post "/v1/agents/$ADA/messages" '{"body":"remember release-day: Thursdays, after the 14:00 sync"}' >/dev/null
sleep 1
post "/v1/agents/$ADA/memory" '{"key":"team","content":"bob reviews code, atlas writes release notes, nova-ops watches deploys."}' >/dev/null || true
curl -sf -X POST "$API/v1/agents/$ADA/memory" -H 'content-type: application/json' \
  -d '{"key":"style","content":"Short answers, links to sources, no jargon."}'
RID=$(post "/v1/agents/$ADA/routines" '{"name":"morning-digest","schedule":"0 9 * * 1-5","prompt":"Summarize what changed since yesterday."}' | json "d['id']")
post "/v1/agents/$ADA/routines" '{"name":"weekly-plan","schedule":"0 8 * * 1","prompt":"Draft the plan for this week."}' >/dev/null
post "/v1/agents/$ADA/routines" '{"name":"friday-wrapup","schedule":"0 17 * * 5","prompt":"Write the weekly wrap-up.","enabled":false}' >/dev/null
post "/v1/routines/$RID/run" '' >/dev/null
sleep 1
post "/v1/agents/$ADA/messages" '{"body":"Thanks! What does `recall` give you for release?"}' >/dev/null
sleep 1
post "/v1/agents/$ADA/messages" '{"body":"recall release"}' >/dev/null
post "/v1/agents/$ATLAS/messages" '{"body":"Draft notes for v0.2"}' >/dev/null
post "/v1/agents/$ATLAS/messages" '{"body":"Also list the new API endpoints"}' >/dev/null
post "/v1/agents/$ADA/snapshots" '{"label":"before-upgrade"}' >/dev/null
sleep 2
curl -sf -X POST "$API/v1/agents/$NOVA/stop"
echo "seeded: ada=$ADA bob=$BOB atlas=$ATLAS nova-ops=$NOVA"
