# CLAUDE.md — ferrobot

Guidance for Claude Code in this repo. Keep it accurate: if a command or path
here stops matching the code, fix this file in the same change.

## Goal

ferrobot is a self-hosted platform for a team of AI agents by Tim Gühnemann.
Each agent gets its own sandboxed computer (container), a mailbox to talk to
other agents, memory, routines (cron), question cards for the operator and a
profile with an animated character avatar. A Rust daemon (`ferrobotd`) owns
all state and logic; a thin desktop app and any other client talk to it over
a versioned HTTP/WebSocket API (`/v1`).

## Clean-room rules (hard)

- OpenBot is PolyForm Noncommercial. Do **not** open, copy, translate or
  paraphrase its code, comments or prompt texts. Ideas and architecture only.
- No Grok Bot / OpenBot logos, icons, brand names, UI texts or 1:1 screens.
  Icons come from lucide (`lucide-preact`); avatars are our own SVG + CSS.
  Reference screenshots are for feel only.
- Do not push, publish, create repos, change remotes or change the repo's
  visibility (github.com/byQuexo/ferrobot) unless asked.
- Never fabricate test results; say what was not run.

## Architecture

```
crates/core      domain types (Agent, mail, mentions, question cards, runner protocol)
crates/store     SQLite (rusqlite, bundled): agents, mailbox, events, memory (FTS),
                 routines, snapshots, forward-only migrations
crates/sandbox   SandboxBackend trait + docker (bollard), local, firecracker
                 (API client; boot untested, needs KVM), sprites backends
crates/mcp       rmcp Streamable-HTTP server: the agents' tools (send_message,
                 ask_user, create_agent, remember, ...), per-agent token auth
crates/daemon    ferrobotd: axum UI API (/v1), agent API (/runner/ws, /mcp, /model),
                 lifecycle, supervisor, scheduler, model proxy, OpenAPI (utoipa)
runner/          TypeScript runner inside each sandbox: drives the model
                 (drivers/claude.ts via Claude Agent SDK, drivers/openai.ts for
                 OpenAI-compatible providers, drivers/codex.ts via `codex
                 app-server`, drivers/acp.ts for ACP agents (Gemini CLI),
                 drivers/claude-code.ts hands turns to the host's own `claude`
                 through the daemon, drivers/mock.ts)
apps/desktop/    Preact + Vite UI in a Tauri shell (own Cargo workspace)
protocol/        generated OpenAPI + JSON Schemas, fixtures, README (the contract)
scripts/         seed-demo.sh, smoke-docker.sh, smoke-live.sh, demo/ (README videos,
                 clips and screenshots: scripts/demo/README.md)
```

Principles:
- **Core crates stay plain**: `core`, `store` and `sandbox` must not
  depend on Tauri or axum (`mcp` serves over axum, never Tauri), so they stay embeddable (e.g. a future UniFFI
  binding). API derives are behind features (`core`/`sandbox`: `schema` for
  utoipa, `core`: `mcp` for schemars). CI checks
  `cargo check -p ferrobot-core -p ferrobot-store -p ferrobot-sandbox`.
- **Desktop is thin**: no business logic in Tauri (there are no Tauri
  commands); the shell only starts the bundled `ferrobotd` (release builds,
  when nothing answers on 127.0.0.1:7878) and injects the operator token. The
  UI does everything through the `/v1` API and the `/v1/ws` event stream;
  the one exception is the updater and process plugins (app update, relaunch).
  Generated API types live in `apps/desktop/src/generated/`.
- **The API is versioned** (`/v1`). Additive changes only; breaking changes
  need a new version. `protocol/` is generated from the code, never by hand.
- Two listeners with different trust: UI API on loopback (operator token),
  agent API reached from sandboxes (per-agent tokens).

## Key entry files

- `crates/daemon/src/main.rs` — CLI flags/env (`FERROBOT_*`), startup.
- `crates/daemon/src/lib.rs` — routers (`ui_router`, `agent_router`).
- `crates/daemon/src/api/` — `/v1` handlers (agents, chat, memory, routines, sandbox, ws).
- `crates/daemon/src/agents.rs` — agent creation shared by API and `create_agent`,
  approval flow, Auto mode, limit; `api/settings.rs` — daemon-wide settings (`auto_mode`).
- `crates/daemon/src/state.rs` — `AppState`, `Settings`, `UiEvent`.
- `crates/daemon/src/spec.rs` — OpenAPI + schema generation.
- `crates/daemon/src/proxy.rs` — model proxy; `auth.rs` — operator token.
- `crates/daemon/src/usage.rs` — token counts read from proxied responses;
  `pricing.rs` + `config/pricing.toml` — prices and cost estimates;
  `api/usage.rs` — `/v1/usage`, `/v1/limits`; `limits.rs` — plan limits
  of the host's Claude Code and Codex sign-ins (60 s cache).
- `crates/daemon/src/api/providers.rs`, `secrets.rs` — model providers and
  their keychain-stored keys; `crates/core/src/provider.rs` — provider kinds.
- `crates/daemon/src/codex.rs` — host-side `codex app-server`: ChatGPT
  sign-in and tokens for `codex` providers (`auth.json` in the keychain,
  a private temp `CODEX_HOME` per run; tokens never enter a sandbox).
- `crates/daemon/src/claude_code.rs` — opt-in Claude Code (your own login): the
  host's own, unmodified `claude -p` per turn (`/host/claude-code/turn`), built-in
  tools off, none of the user's settings, the sandbox reached through MCP tools
  (`bash`, `read_file`, `write_file`); `GET /v1/claude-code` reports its login state;
  the operator's hooks and plugins (`/v1/claude-code/settings`) go in as `--settings`
  and `--plugin-dir`.
- `crates/daemon/src/tool_check.rs` — the rule-based tool-call classifier: safe, risky
  (an approval card unless Auto mode is on) or dangerous (refused); gates connector calls,
  Claude Code's `bash` and, via `/runner/tool-check` (`runner/src/gate.ts`), every driver's shell.
- `crates/daemon/src/connectors.rs`, `api/connectors.rs` — connectors: the daemon
  is the MCP client of remote MCP servers, and of local ones it starts on this
  computer over stdio (`command`, a cleared environment), and lends their tools to assigned
  agents through `/mcp` (tokens in the keychain, never in a sandbox), and the
  `add_connector` approval card (Approve/Decline; tokens go in Settings → Vault);
  `oauth.rs` — their OAuth sign-in (rmcp's client, loopback callback).
- `crates/daemon/src/clis.rs`, `api/clis.rs` — CLIs: keyless Debian packages
  built into one shared sandbox image (Docker, `build_image` in
  `crates/sandbox`); `lifecycle::wake` recreates outdated sandboxes, home kept.
- `crates/daemon/src/voice.rs`, `api/voice.rs` — voice: speech and
  transcription for the operator's chat via ElevenLabs or OpenRouter (keys
  in the keychain as `voice:<provider>`); `call.rs` — the operator's voice
  call (one agent: in its chat; two or more: a room (`crates/store/src/rooms.rs`)
  where every line is due for every other member and they take the floor one
  at a time, host first, each speaking or passing, four agent lines in a row at
  most; room messages carry `room_id` and stay out of chats; the call note goes
  to the model at hand-off, the host also gets the host note; `hang_up` takes
  effect when the agent's turn ends, and the call's end cuts off the room turn
  holding the floor and drops queued ones; `ask_user` is refused in a call;
  `call_agents` starts a call among agents without the operator, which ends once
  nobody is due (12 agent lines in a row at most) and which the operator can join);
  `apps/desktop/src/lib/voice.ts` — playback and recording in the UI;
  `lib/call-session.ts` — the call in the UI (outlives the chat),
  `lib/call.ts` — its sentence, barge-in and call-event rules.
- `crates/daemon/src/api/groups.rs` — group chats: named rooms that talk by the
  room rules outside a call (`call.rs`, the group note); calling one makes it
  the call's room. UI: `apps/desktop/src/components/Groups.tsx`, `Rooms.tsx`.
- `crates/daemon/src/storage.rs` — object storage (`object_store`) for snapshot
  archives, `--storage-url` (default: the data dir; `s3://` for S3/R2).
- `crates/daemon/src/update.rs` — `/v1/update` status; `update/source.rs` — a
  debug build pulls `main`, rebuilds and re-execs itself (debug builds only).
- `crates/daemon/src/lifecycle.rs`, `supervisor.rs`, `scheduler.rs`, `runner_ws.rs`.
- `crates/daemon/src/trace.rs` — a turn as a timeline (model calls from usage rows tagged
  with the turn, tool calls from its events, waits from the cards stamped with its
  `turn_id`, handoffs from its `handoff` events, which `enqueue` in `crates/mcp/src/tools.rs`
  and `tasks::report` record); `GET /v1/agents/{id}/turns/{turn_id}/trace`.
  `otel.rs` — sends each finished turn's trace to the operator's OTLP/HTTP endpoint
  (hand-encoded protobuf; `/v1/otel`, header value in the keychain as `otel:header`).
- `crates/daemon/src/webhooks.rs` — incoming webhooks (`POST /hooks/{id}/{secret}` on the
  UI listener, outside `/v1` and its auth; a message from `Sender::Webhook`, framed as
  external data) and outgoing ones (events posted as Discord, Slack or JSON; URLs in the
  keychain as `webhook:<id>`); `api/webhooks.rs` — `/v1/webhooks`. All in the `webhooks`
  setting, no table of their own.
- `crates/daemon/src/discord.rs` — the Discord bridge: polls bridged channels through
  Discord's REST API with the operator's bot token (keychain `discord:bot`); messages
  from allowed Discord users go to the channel's agent as the operator's, replies go back
  (👀, then ✅, or only the agent's emoji); an agent posts to its own bridged channels
  with `send_to_discord`; `discord/cards.rs` — cards in the channel (✅/❌, replies);
  `api/discord.rs` — `/v1/discord`. Settings in the `discord` setting.
- `crates/daemon/src/flows.rs`, `api/flows.rs` — flows (proof of concept): linear steps (agent
  turn, condition, message) in the `flows` setting, run by hand; an agent step waits for its
  message's turn in `runner_ws::settle`; runs in memory.
- `crates/daemon/src/stop.rs` — the operator's Stop (`interrupt?chain=true`): the agent's turn,
  the turns of agents working on messages from that chain, and agent messages queued among them.
- `crates/daemon/src/tasks.rs` — tracked tasks: automatic report and reminder;
  `crates/store/src/tasks.rs` — task state from `task` events and the mailbox.
- `crates/mcp/src/tools.rs` — MCP tools; `crates/mcp/src/lib.rs` — `MailboxNotifier`.
- `crates/store/src/migrations.rs` — schema migrations; `vault.rs` — memories as
  Markdown notes in an Obsidian vault when the `memory_vault` setting is set
  (`/v1/memory-vault`); `memory.rs` routes there.
- `crates/core/src/question.rs` — question/approval cards; `tools.rs` — tool groups.
- `runner/src/index.ts`, `runner/src/runner.ts`, `runner/src/drivers/`.
- `apps/desktop/src/app.tsx`, `src/api.ts`, `src/components/`, `src/lib/timeline.ts`.

## Code conventions

The rules are in [`docs/CONVENTIONS.md`](docs/CONVENTIONS.md); the points below
are the repo-specific ones.

- Rust 2024, stable toolchain (`rust-toolchain.toml`), `rustfmt.toml`.
- `unsafe_code = "forbid"`; clippy `pedantic` is on and CI uses `-D warnings`.
  Fix lints; an `#[allow(...)]` needs a `reason = "..."`.
- Errors: `thiserror` in libraries, `ApiError` in the daemon (JSON
  `{"error", "message"}`); `anyhow` only in `main.rs`.
- Every API type/route gets utoipa annotations with doc comments; they become
  the public spec.
- Tests: unit tests next to the code; daemon integration tests in
  `crates/daemon/tests/` (`features.rs`, `e2e.rs`, `specs.rs`, `protocol.rs`).
  New behaviour gets a test.
- TypeScript: strict, ESM, `node --test` tests; runner is linted with eslint.
- UI text is English, short and plain.

## Commands

GitHub Actions minutes are limited: run these gates locally before a merge
(including the Tauri shell's clippy when `src-tauri` changed). CI runs only
the Rust and TypeScript checks on PRs; E2E runs by hand ("Run workflow").

Rust (repo root):
```
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo check -p ferrobot-core -p ferrobot-store -p ferrobot-sandbox
```

Spec drift (`protocol/` must match the code):
```
cargo test -p ferrobot-daemon --test specs                          # check
FERROBOT_UPDATE_SPECS=1 cargo test -p ferrobot-daemon --test specs  # regenerate
cd apps/desktop && npm run gen:api     # regenerate TS types; check:api = gen + git diff
```

End-to-end:
```
cargo test -p ferrobot-daemon --test e2e -- --ignored                # local backend (needs runner/dist)
docker build -f runner/Dockerfile -t ferrobot-runner:dev .           # runner image
cargo test -p ferrobot-sandbox --features docker-tests --test docker
cargo test -p ferrobot-daemon --features docker-tests --test e2e -- --ignored
./scripts/smoke-docker.sh            # real ferrobotd + Docker + mock driver
./scripts/smoke-live.sh              # real Claude + Ollama turns, key-leak check (asks for the key, hidden)
sudo scripts/firecracker/build-rootfs.sh   # Linux only; see scripts/firecracker/README.md
```

Runner (`runner/`): `npm ci && npm run lint && npm run format:check && npm run typecheck && npm test && npm run build`

Desktop (`apps/desktop/`): `npm ci && npm run lint && npm run format:check && npm run check:api && npm run typecheck && npm test && npm run build`;
Tauri shell: `cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings`.
The built `dist/` is embedded at compile time: run `npm run build` before
`cargo build` in `src-tauri`.

Release bundle (local, host arch; CI builds universal macOS and Windows on a
`v*` tag, `.github/workflows/release.yml`):
```
cargo build --release -p ferrobot-daemon
mkdir -p apps/desktop/src-tauri/binaries
cp target/release/ferrobotd apps/desktop/src-tauri/binaries/ferrobotd-$(rustc -vV | sed -n 's/^host: //p')
cd apps/desktop && npm run tauri build -- --bundles dmg --config src-tauri/tauri.release.conf.json \
  --config '{"bundle":{"createUpdaterArtifacts":false}}'
```
The release config makes signed updater artifacts: to build them, drop the last
`--config` and set `TAURI_SIGNING_PRIVATE_KEY=~/.tauri/ferrobot-updater.key` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

Demo data:
```
FERROBOT_DATA_DIR=/tmp/fbdemo ./target/debug/ferrobotd --backend docker --driver mock --idle-timeout 0
FERROBOT_DATA_DIR=/tmp/fbdemo bash scripts/seed-demo.sh [avatar.png]
```
Clean up afterwards: delete demo agents (`DELETE /v1/agents/{id}`), stop the
daemon, `docker ps -a` must be empty of ferrobot containers.

## Migrations

- `crates/store/src/migrations.rs`, tracked with `PRAGMA user_version`.
- Forward-only, numbered v1..v22 (v7: `providers`, `agents.provider_id`/`model`; v8:
  `global_memories`; v9: `connectors`, `agent_connectors`; v10:
  `global_memories.author`; v11: `agents.pinned`; v12: `settings`, JSON by
  key, e.g. `voice`; v13: `usage`, one row per proxied model call; v14:
  `usage.cost_usd`, OpenRouter's reported charge; v15: `agents.tools`, NULL =
  every tool group; v16: `rooms`, `room_members`, `room_lines`,
  `messages.room_id`, index `events_turn`; v17: `room_lines.invited`, NULL =
  words said; v18: `agents.tags`, NULL = none, index `usage_agent`; v19:
  `rooms.name`, NULL = a call room, else a group chat; v20: `usage.turn_id`,
  `usage.at_ms`, index `usage_turn`; v21: `agent_aliases`, old names of
  renamed agents; v22: `connectors.command`, `connectors.token_env`, NULL = a
  remote server). Append a new entry
  with a `// vN: ...` comment; **never edit or reorder existing ones**.
- Every migration gets a test (upgrade an old DB, check defaults of existing
  rows). New columns on existing tables need a default.
- An older daemon cannot open a newer DB; mention new migrations as a
  notable change in reports.

## Must not break

- **Model keys stay in the daemon.** `ANTHROPIC_API_KEY` and provider keys
  (OS keychain via `secrets.rs`, never SQLite) are read by `ferrobotd` only.
  Sandboxes reach the model through `/model` with their agent token; the
  proxy picks the upstream from the agent's provider and swaps in the real
  key. Never pass a key into a sandbox, env, log, event or API response.
  Connector tokens follow the same rule: only the daemon talks to connectors.
  Claude credentials are never read, stored or relayed: no Claude sign-in in
  ferrobot; the opt-in Claude Code provider runs the host's own, unmodified
  `claude` (built-in tools off, none of the user's settings: only the hooks and
  plugins the operator sets in Settings → Claude Code, which agents cannot
  change; the operator's Subagents switch adds only `Task`) and reaches the
  sandbox only through MCP. Connector OAuth is fine.
- **Operator token on `/v1`.** Every UI route requires
  `Authorization: Bearer <token>` (`FERROBOT_API_TOKEN` or
  `<data dir>/api-token`); only `/v1/health` and `/v1/oauth/callback` (single-use
  `state`) are open, and `/artifacts/` takes signed links. Keep the UI listener on
  loopback by default.
- **Incoming webhooks open nothing else.** `/hooks/{id}/{secret}` sits outside the
  `/v1` auth layer and takes only its own secret (hash stored, URL shown once at
  create or regenerate); the operator token is not accepted there and a webhook
  secret opens no other route. Its body reaches the agent framed as external data,
  never as the operator's words; agents never see the secret.
- **The Discord bridge listens only to allowed users.** Only messages from the Discord
  user ids the operator listed reach an agent (as the operator's); everyone else and all
  bots are ignored. The bot token stays in the secret store and the daemon: never in a
  sandbox, event, log or API response. Cards answered from Discord take only allowed
  users' reactions or replies and go through the app's answer route; cards with a secret
  field or several questions, `add_connector` and very risky steps stay app-only.
- **Per-agent tokens** on `/runner/ws`, `/mcp`, `/model`, `/host/claude-code/turn`. An agent can only act
  as itself (MCP tools read the caller from the token). Only hashes are stored.
- **`create_agent` needs operator approval unless Auto mode is on**: an
  Approve/Decline card, the tool waits; decline or `FERROBOT_APPROVAL_TIMEOUT`
  (300 s) creates nothing. Auto mode (`PUT /v1/settings`, operator opt-in)
  skips the card and leaves an `agent_created` notice; the agent limit applies either way.
  `create_routine` (an agent's own routine) follows the same rule, without waiting.
- **Tool profiles are enforced, not just hidden.** The daemon filters and refuses
  (`tools/call` too) ferrobot tools outside an agent's profile; the runner turns
  off built-in groups per driver. A new agent's tools and connectors are at most
  its creator's, checked in the daemon in every mode, Auto mode included.
  `set_agent_tools` changes only agents the caller created (never itself), to at most
  the caller's own; narrowing applies at once, anything new needs a card unless Auto mode is on.
- **Risky steps need a card unless Auto mode is on; critical ones (money or deleting
  through a connector, force-pushing, publishing) always do; dangerous ones are always
  refused** (`tool_check.rs`). The runner asks before every shell command and
  fails closed when the check can't answer.
- **`FERROBOT_MAX_AGENTS`** (default 10) applies to `POST /v1/agents` and to
  `create_agent` (both through `daemon::agents::create`).
- Question cards are answered at most once (`409` after).
- **Local MCP servers run on the host only on the operator's word**: added in
  Settings, or through an agent's `add_connector` card that shows the exact
  command; Auto mode never skips that card. They start without a shell, with a
  cleared environment (`PATH`, `HOME`, user, locale, temp, and the connector's
  Vault token under `token_env`), so no model key or operator token reaches them.
- **No card asks for a key.** An agent's `add_connector` card is
  Approve/Decline only; connector tokens are typed in Settings → Vault. Agent
  cards cannot carry `secret` fields (`ask_user` refuses them), the answer
  route strips any secret value before anything is recorded, broadcast or
  sent, and agents can never refer to stored secrets.

## Open items

- Approval timeout (300 s) is untested with the real Claude MCP client (it may
  time out tool calls earlier).
- Docker e2e, `smoke-docker.sh` and the Docker sandbox test passed on
  `feature/firecracker-client` (macOS, Docker Desktop); runner image rebuilt.
- Firecracker: no microVM has booted; every `TODO(kvm)` needs a KVM host.
- Anthropic (Claude driver), Ollama and OpenRouter (Tencent's hy4-flash, in
  daily use since 2026-11-20) have run against a real upstream; OpenAI,
  Gemini, Bedrock and Vertex never have.
- Codex 0.166.0 is in the runner image; a live Codex turn ran against Ollama's
  Responses API, not against OpenAI (no key in the environment).
- A live Claude turn passed (`scripts/smoke-live.sh`, 2026-11-20, macOS): an
  Anthropic provider with its key in the macOS Keychain, a tool call in a
  Docker sandbox, no trace of the key in the sandbox, logs or data dir.
  Claude steering is still untested against the real API; the Windows
  Credential Manager path has never run.
- Voice ran live with ElevenLabs in the Tauri window (calls, rooms, streamed
  speech, live transcription; 2026-11-21); OpenRouter voice only against a
  fake upstream.
- Claude Code (your own login) ran live on macOS (2026-11-23, CLI 2.1.330,
  claude.ai login, Docker sandbox); Windows never.
- The app updater never ran against a real release (v0.1.0 shipped without
  update files), and its Windows install path is untested.
- Claude Code hooks, plugins and Subagents (2026-11-24) ran only as direct
  `claude -p` turns (Haiku) with the daemon's flags, never through a daemon
  turn; that a `Task` subagent has no built-in tools was not observed, and its
  model calls likely miss Usage and traces.
- OTLP export ran against Grafana's collector and Tempo (root span only;
  child spans only in a unit test), never against Phoenix or Langfuse.
- The Obsidian vault memory ran only against a temp folder, never with
  Obsidian open; frontmatter with CRLF line endings is not parsed.

## Next up: Firecracker

Roadmap: `docs/wiki/Roadmap.md`.
Status per phase: `docs/progress.md`; assumptions: `docs/decisions.md`.

1. ~~Storage layer on `object_store`~~ (done: snapshot archives, `--storage-url`).
2. Firecracker spike on a KVM host (API client, boot sequence, init and
   rootfs script exist; never booted).
3. Firecracker backend behind the `SandboxBackend` trait (written, `TODO(kvm)`:
   process spawn, TAP, snapshots; exec/files via the vsock guest agent,
   tested only against a mock).
4. Jailer for isolation.
5. ~~Warm/cold sleep with a `SleepLevel` enum~~ (done for Docker: pause/stop).
