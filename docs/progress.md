# Progress

Status per roadmap phase (the [roadmap page](https://cursor.com/canvas/store-0b9dda1f-693e-4629-9899-c87852755bdc/ad9f72df-2257-4c5f-ba31-48368896c60a)). Decisions are in
`docs/decisions.md`.

## M1 Storage layer — done (`feature/m1-storage`)

**Done**

- `object_store` 0.14.2 (MIT/Apache-2.0) behind `--storage-url` /
  `FERROBOT_STORAGE_URL`; default is the local data dir, `s3://…` covers
  S3 and R2, `file://` and `memory://` work too (`crates/daemon/src/storage.rs`).
- Docker snapshot home archives go through the store
  (`DockerConfig::snapshot_store`) instead of a fixed directory.
- No MinIO, no AGPL crate added.

**Tested**

- Put/get/delete on the local FS, on a `file://` URL and on `memory://`;
  an `s3://` URL builds offline (`storage::tests`).
- Docker snapshot → restore → delete through the store:
  `cargo test -p ferrobot-sandbox --features docker-tests`.

**Open**

- Not run against a real S3 or R2 bucket (needs credentials).
- Home archives are buffered in memory and sent as one PUT (S3 limit 5 GiB);
  see the `shortcut:` comment in `docker.rs`.

## Phase 2: Providers and drivers (`feature/providers`)

**Done**

- Provider kinds: Anthropic, OpenAI, Gemini, Bedrock, Vertex, OpenRouter,
  Ollama, generic OpenAI-compatible (`crates/core/src/provider.rs`).
- Migration v7: `providers` table, `agents.provider_id` and `agents.model`.
- `/v1/providers` CRUD; keys only in the secret store (OS keychain by
  default, `--secret-store file` for CI), never returned (`has_key` only).
- Agents pick provider and model (`POST`/`PATCH /v1/agents`); a change
  rebuilds the sandbox with the new env, disk kept. Agent-created agents
  inherit the creator's choice.
- Model proxy routes per agent: upstream from the provider, credential
  injected per kind (`x-api-key`, Bearer key, Bedrock API key, gcloud token).
- Runner: new OpenAI-compatible driver (streaming, tool loop with `bash`,
  `read_file`, `write_file` and the ferrobot MCP tools, permission policy,
  persisted sessions, abort). No new npm dependency.
- Desktop: "Model providers" dialog (key icon in the sidebar), provider and
  model fields in the agent dialog.

**Tested**

- Rust: provider kinds/validation, store CRUD + in-use guard, migration v7
  upgrade, secret stores (memory, file with mode 0600), and a daemon test
  per kind that checks sandbox env (no secret inside) and the credential the
  upstream receives (`features.rs`: `providers_keep_keys_in_the_daemon`).
- Runner: 14 new tests for the driver (fake fetch and MCP).
- End to end by hand: real `ferrobotd` (local backend) + real runner with the
  openai driver + a fake OpenAI-compatible server. A message ran a `bash`
  tool call and streamed the answer back; the upstream saw the provider key,
  the key never appeared in the sandbox directory or the daemon log.
- Bedrock and Vertex request shapes were checked by capturing the bundled
  Claude binary's requests against a local server (no AWS/GCP calls).

**Open**

- Not run against any real provider (needs keys): OpenAI, Gemini,
  OpenRouter, Ollama, Bedrock, Vertex, Anthropic via a provider.
- Gemini's OpenAI-compatible endpoint with our tool schemas is untested
  (schemas are flattened, `$defs` inlined, for that reason).
- Bedrock supports API keys only (no IAM/SigV4); Vertex needs `gcloud` logged
  in on the daemon host.
- OpenAI driver: history is never trimmed (`shortcut:` comment); no audit log
  lines like the Claude driver writes.
- Runner image must be rebuilt for Docker to have the new driver.
- The real OS keychain was never exercised: tests, e2e and the UI check used
  the memory or file store. An ad-hoc-signed `ferrobotd` will likely trigger
  a macOS Keychain prompt after each rebuild.
- Existing, not new: `Settings` derives `Debug` and would print
  `anthropic_api_key` if someone formatted it; nothing does today. The new
  token caches (gcloud, ChatGPT) have redacted `Debug` (`debug_never_shows_tokens`).
- Desktop dialogs: looked at once in a browser against a real daemon (mock
  driver); not in the Tauri window.

## Phase 3: Codex driver (`feature/codex-driver`)

**Done**

- Provider kind `codex`; agents on it get the new `codex` runner driver.
- Runner `drivers/codex.ts`: one `codex app-server` per runner, thread
  start/resume (session id = thread id), turns, streaming text, command /
  patch / MCP items as tool events, approvals through `decide()`, interrupt,
  failed turns, clear error when `codex` is missing.
- Daemon: proxy routing with an OpenAI key or the host's ChatGPT token
  (+ account header); `POST/GET /v1/providers/{id}/chatgpt-login` (device
  code sign-in through the host's `codex`, status polling); `--codex-bin`.
- Desktop: "Sign in with ChatGPT" on codex providers, showing URL and code.

**Tested**

- Runner: 13 tests against a scripted fake app-server (handshake, resume,
  approvals allowed/refused, command items, failed turn, interrupt, missing
  binary, generated config without secrets, config validation).
- Daemon: `codex_providers_use_a_key_or_the_hosts_chatgpt_login` with a fake
  host `codex` (shell script): key path, device-code sign-in, token + account
  header injection, agents cannot set the account header.
- Desktop dialog looked at in a browser against a real daemon: the sign-in
  button and a missing host `codex` are shown plainly.

**Open (needs Tim)**

- No real `codex` binary was run, in the sandbox or on the host. The runner
  image does not include it; adding `@openai/codex` is a new npm dependency.
- A live ChatGPT sign-in and model call need Tim's ChatGPT account. Whether
  the ChatGPT backend accepts requests from a custom-provider Codex with only
  bearer + account headers is unverified.
- Unverified protocol details are listed in the driver's report: `move_path`
  in file-change items, approvals without a `command`, Codex calling proxy
  paths other than `/responses`.

## Phase 4: Warm and cold sleep (`feature/sleep-levels`)

**Done**

- `SleepLevel` in `SandboxBackend` (`sleep`), `SandboxState::Paused`,
  `AgentStatus::Paused`; Docker pause/unpause; supervisor promotes
  Running → Paused (`--warm-after`) → Stopped (`--idle-timeout`) and never
  treats Paused as a crash; reconcile keeps paused agents paused.
- API: `POST /v1/agents/{id}/stop?level=warm|cold`, `warm_after_s` in the
  sandbox info; OpenAPI and TS types regenerated.

**Tested**

- Daemon: `idle_agents_pause_before_they_stop` (warm, no restart while
  paused, cold after the idle timeout, operator pause, message resumes
  without re-creating), `backends_without_pause_stay_awake_until_the_cold_timeout`
  (local backend), the existing idle/crash test, and a new check that every
  OpenAPI `$ref` resolves.
- Docker (real engine): pause → `Paused` → wake → `Running`; stop while
  paused; pausing a stopped container reports `Cold`.

**Open**

- A long pause with a real runner (WebSocket, model connections) was not
  measured. Firecracker Warm/Cold (F4a/F4b) needs KVM; see phase 5.

## Phase 5: Firecracker, as far as it goes without KVM (`feature/firecracker-client`)

**Done**

- `crates/sandbox/src/firecracker/`: API client (machine config, boot source,
  drives, network, MMDS, vsock, actions, pause/resume, snapshot create/load,
  instance info), pure boot-sequence builder, `FirecrackerBackend`
  (create/wake/stop/sleep/destroy/state; exec/files/snapshots
  `NotImplemented`).
- `--backend firecracker` with `--fc-bin`, `--fc-kernel`, `--fc-rootfs`,
  `--fc-host-ip`, `--fc-bridge`; refuses clearly without `/dev/kvm`.
- `scripts/firecracker/`: `build-rootfs.sh` (runner image → ext4 with init),
  `ferrobot-init` (PID 1: mounts, home disk, env from MMDS, runner as `node`,
  power-off), README with host requirements and firewall rules.
- `supports_snapshots()` on the trait.

**Tested (macOS, no KVM)**

- 17 new sandbox tests: every client call's verb/path/body against a fake
  API on a Unix socket, error mapping (`fault_message`), the exact boot
  sequence (env only in MMDS), the KVM refusal, state mapping, pause/resume,
  disk keep/purge.
- The init's MMDS env reading ran inside `ferrobot-runner:dev` against a fake
  MMDS (curl and node paths). Scripts checked with `bash -n`/`sh -n`/`dash -n`
  (no shellcheck installed).
- Whole stack: 102 Rust tests, Docker sandbox test, Docker e2e and
  `smoke-docker.sh` all pass on this branch.

**Open (`TODO(kvm)`, 13 places)**

- No microVM has booted: process spawn, TAP setup, guest kernel `ip=`,
  init, MMDS from the guest, runner reaching the daemon.
- Guest agent for exec/files (F2), jailer (F3), snapshot sleep (F4b).

## Live smoke test (`feature/live-smoke`)

Real `ferrobotd` with the Docker backend on macOS (Docker Desktop), one
tool-using turn per driver, runner image built from this tree.

| Driver | Model | Result |
|---|---|---|
| `openai` | Ollama 0.12.6 in a container, `qwen2.5:3b`, provider `openai_compatible` | `bash` tool ran in the container, answer streamed back |
| `codex` (Codex 0.166.0 in the image) | Ollama 0.40.16 `/v1/responses`, `qwen2.5:3b`, provider `codex` with base URL override | shell tool ran, answer streamed back |
| `claude` (Agent SDK in the image) | fake Anthropic upstream (`--anthropic-upstream`), canary key in the daemon env | `Bash` tool ran, answer streamed back; the upstream saw the daemon key, never the agent token |

Leak check for each run: a canary key on the provider (or in the daemon env)
was searched in the container env (`docker inspect` and `/proc/*/environ`),
every file in the container, the container logs, the daemon log and the
SQLite files: never found. It is only in `secrets.json` (the file secret
store used for the run) or the daemon's own env.

**Fixed because of it**

- Codex 0.166 rejects `approval_policy = "untrusted"`; now `on-request`.
- `/v1/health` reported `model_proxy: false` while the proxy served providers.

**Not done**

- No call to the real Anthropic API: `ANTHROPIC_API_KEY` is not in this
  session's environment (launchd has the name with an empty value). Put the
  key in `~/.config/ferrobot/anthropic.key` (mode 0600) and the same run takes
  a minute.
- No real OpenAI key either (`OPENAI_API_KEY` unset), so the L3 turn ran
  against Ollama's Responses API instead of OpenAI; a ChatGPT sign-in was not
  tried.

## L4: ACP driver, Gemini CLI first (`feature/acp-driver`)

**Done**

- Provider kind `gemini_cli` (driver `acp`): the sandbox gets the agent
  token as `GEMINI_API_KEY` and the proxy as `GOOGLE_GEMINI_BASE_URL`; the
  proxy accepts the token from `x-goog-api-key` and sends the real key the
  same way to `https://generativelanguage.googleapis.com`.
- Runner `drivers/acp.ts`: ACP client over stdio (`initialize`,
  `authenticate`, `session/new` with the ferrobot MCP server over HTTP,
  `session/load` for resume, `session/prompt`, `session/update` → turn
  events, `session/request_permission` → `decide()`, `session/cancel`).
- `@google/gemini-cli@0.67.0` in the runner image.
- MCP server is now stateless (`legacy_session_mode = false`).

**Tested**

- Runner: 16 tests against a scripted fake ACP agent; the subagent also ran
  the driver against the real `gemini --acp` 0.67.0 offline (fake Gemini API).
- Daemon: `gemini_cli_agents_reach_the_gemini_api_through_the_proxy`.
- Live in Docker through the real daemon proxy and a fake Gemini API: a
  `run_shell_command` call ran in the container and the ferrobot `remember`
  MCP tool stored a memory (the roadmap's "mailbox remains reachable").
  The fake upstream saw only the provider's canary key; the canary is in no
  container env, file or log, and the agent token is not written to disk.

**Fixed because of it**

- Gemini CLI dropped every ferrobot tool: after `tools/list` it opens the
  standalone GET stream, rmcp answered with an empty body, the client
  disconnected. The MCP server now runs stateless (no sessions, no GET
  stream). Re-checked after the switch: the runner's own client (e2e,
  smoke test), rmcp's client incl. `create_agent` (`mcp_e2e.rs`), and the
  Claude SDK in Docker calling `mcp__ferrobot__remember` against a fake
  Anthropic upstream. Not re-run: Codex's MCP client, and a long (300 s)
  `create_agent` approval wait.

**Open**

- No real Gemini model (no `GEMINI_API_KEY` in the environment).
- Gemini 0.67 runs some commands without asking, so `decide()` only sees what
  it asks about; its shell results reach the timeline as "completed" only.
- The `session/load` replay drain is timing-based (200 ms quiet, max 5 s).

## F4c: archive for Docker (`feature/docker-archive`)

**Done**

- `SleepLevel::Archived`; Docker `sleep(Archived)` streams the home
  directory and the committed image (`docker image save`) into the object
  store as `archives/<name>/home.tar` and `image.tar` (multipart, never fully
  in memory), then removes container, volume and image.
- `create` restores an archived sandbox (load image, create container,
  unpack home, start) and deletes the archive; so a wake (NotFound →
  provision) brings it back without daemon changes to the wake path.
- `AgentStatus::Archived`; supervisor archives stopped agents after
  `--archive-after` / `FERROBOT_ARCHIVE_AFTER` (default 0 = off);
  `POST /v1/agents/{id}/stop?level=archived`; deleting an agent with its data
  deletes its archive; info panel shows "archived, restores on wake".
- The token inside the archived image's env is retired at archive time.

**Tested**

- Docker (real engine): archive → container gone, both objects in a local
  store → `create` restores, home file intact, archive consumed
  (`archive_moves_the_sandbox_into_the_store_and_back`).
- Daemon: `idle_sleeping_agents_are_archived_and_restored_on_wake` (supervisor
  archives after the timeout, old token rejected, message restores with a
  fresh token, delete removes the archive).
- Live: a real agent archived through the API (11.6 s, `image.tar` 1.29 GB,
  `home.tar` 188 KB) and restored by an `exec`, its file intact.

**Open**

- ~~No encryption at rest~~ (done in the hardening pass, item 3).
- Each archive saves the whole image (~1.3 GB); see the `shortcut:` comment.
- Firecracker archive (snapshot + disk) is part of F4b/F4c on KVM.

## F2: guest agent over vsock (`feature/guest-agent`)

**Done**

- Host: `crates/sandbox/src/firecracker/guest.rs` connects to the VM's vsock
  socket, does the `CONNECT 52` handshake, sends one JSON request and reads
  one answer; `exec`, `read_file`, `write_file` of the Firecracker backend use
  it. Boot sequence adds `PUT /vsock` (cid 3, `<vm dir>/vsock.sock`).
- Guest: `runner/src/guest-agent.ts` (built into the runner image as
  `dist/guest-agent.js`): exec with cwd/env and 1 MiB output caps, binary
  file read/write, `not_found` errors. The init starts
  `socat VSOCK-LISTEN:52,fork` as `node`; the rootfs build adds `socat`.

**Tested**

- Host side against a Unix-socket mock of Firecracker's vsock (handshake,
  exec, binary read/write, not found, refused port, missing socket).
- Guest side as functions and as a stdio program, the way socat runs it.
- Debian bookworm's socat 1.7.4.4 has the `VSOCK-LISTEN` address.

**Open (`TODO(kvm)`)**

- Never ran inside a microVM: vsock device, socat in the guest, runner image
  in the rootfs.

## Agent briefing, mentions, global memory, connectors (`feature/briefing-memory-connectors`)

**Done**

- `create_agent` takes a `task`, sent as the creator's first message to the
  new agent; the shared prompt sends a briefed agent's questions and results
  back to its creator, who relays operator questions with `ask_user`.
- Agents are told to mention teammates as `@name` (rendered as chips); code
  spans show tokens as `@name`, a lone mention in backticks is still a chip.
- Global memory (migration v8): `remember`/`forget` with `global: true`,
  `recall` searches both scopes; `/v1/memory`; Global memory dialog.
- Connectors (migration v9): `/v1/connectors`, `/v1/agents/{id}/connectors`,
  daemon-side MCP gateway, Connectors dialog, per-agent checkboxes.

**Tested**

- MCP e2e: the task arrives as a message from the creator with mentions as
  tokens; global memory across two agents, scopes alternate, limit holds.
- Daemon: a real upstream MCP server that accepts only `Bearer secret`;
  the assigned agent lists and calls its tools, the upstream saw only
  `Bearer secret`, the unassigned agent neither sees nor can call them,
  created agents inherit, deleting removes token and tools.
- Store: v8/v9 upgrade, global memory, connector CRUD and assignment.

**Not tested**

- No live model turn with the new prompt (whether models now pass `task` and
  write `@name`).
- No real third-party connector (GitHub, Linear, ...); OAuth-only servers
  cannot be added yet.
- Connector tool lists in Codex and Gemini CLI after an assignment change.

## Connector OAuth, CI fix (`feature/connector-oauth`)

**Done**

- OAuth sign-in for connectors: `POST/DELETE /v1/connectors/{id}/oauth`,
  `GET /v1/oauth/callback`, Sign in / Sign out in the Connectors dialog.
- Bold or backticked agent names become mention chips (models ignore `@`).
- Runner tests no longer hang in CI: the fake Codex sends `turn/started`
  like Codex does; `node --test` has a 30 s per-test timeout.

**Tested**

- Daemon against a fake authorization server and MCP server: discovery,
  registration, PKCE checked at the token endpoint, callback without the
  operator token, single-use `state`, nothing echoed, access token used
  upstream, tokens never in API responses, sign-out removes the tools.
- Runner suite 3x on Linux with `--cpus=0.5` (failed every run before).

**Not tested**

- A real OAuth provider (Linear, Notion, ...), token refresh against one, and
  the sign-in link in the Tauri window.

## Chat and safety follow-ups (`feature/connector-oauth`, continued)

**Done**

- Security review (three subagents) and fixes: model proxy allowlist per API
  family, plain paths below the base, no redirects, header allowlists;
  global memory authors (v10); connector connect timeout; secrets tmp file;
  clean `exec` env on the local backend; OAuth fixes (see decisions).
- Sub-agent questions go to the creator as a message; sub-agent approval
  cards appear in the operator's chat (top creator); approval cards show
  instructions and inherited connectors and no redundant agent name.
- Chat: Markdown, current step instead of typing dots, tool calls and turn
  errors moved to the agent's Activities tab, bold/backticked names as chips.
- Steering: operator messages sent during a turn reach that turn (OpenAI-
  compatible driver), agents are told to acknowledge them at once.
- Agents are told to share findings as they go.
- Desktop links open in the system browser.

**Tested**

- Daemon: proxy escape requests 404 through the real router; steering with a
  fake runner (seen message delivered, unseen one requeued as next turn);
  sub-agent approval card in the creator's chat; OAuth without metadata
  refused. MCP: sub-agent `ask_user` becomes a message to the creator; agents
  cannot overwrite others' global memories. Runner: late messages in the
  OpenAI driver, `steered` reported by the runner. Desktop: Markdown parser,
  step labels.

**Not tested**

- None of the UI changes in the Tauri window (Activities tab, step line,
  Markdown, link opening); steering and the new prompt lines with a real
  model; Codex and Gemini CLI do not steer (Claude does since item 6).

## Hardening after the external review

- **Item 1 (shared files):** done. Links are HMAC-signed and expire after
  30 minutes (key in the keychain); the page CSP allows no remote origin;
  `files::read` only reads the event's own agent's `artifacts/<agent>/<uuid>`.
  Tested in `agents_share_files_that_open_from_storage`: unsigned, tampered,
  expired and other-file links 404; forged events with another agent's key
  or a `../` key 404 on both routes. Not checked: the Tauri window with the
  new links (the URL shape changed; FileViewer appends `&download=1`).
- **Item 2 (Settings Debug):** done; test `debug_never_shows_the_api_key_or_token`.
- **Item 6 (steering, tool refresh):** Claude steers via the SDK's streaming
  input; Codex and Gemini documented as not steering. A connector assignment
  re-sends `configure`; Codex and Gemini start a fresh process next turn.
  Tested with a fake SDK `query()` (late message folded in with
  `priority: "next"` and `client_composed`, input ends once all are answered,
  a message after the answer keeps the query open) and fake Codex/Gemini
  binaries (a reconfigure starts a new process); the daemon sends `configure`
  on assignment. Not tested against the real Claude CLI (does it fold
  `"next"` messages as documented?), Codex or Gemini.
- **Item 3 (archives):** encrypted with AES-256-GCM, key in the keychain;
  non-local `--storage-url` refused without the key; old token rotated
  (401 on `/mcp`) and blanked in committed images. Tested: seal round trip at
  every chunk boundary and in any piece sizes; wrong key, other object path,
  dropped/appended chunk, flipped byte, missing key all fail; plaintext only
  where allowed; key made once, a corrupt key never replaced; `Debug` of the
  key redacted. The Docker archive test (`docker-tests`) now checks the
  header on disk, no plaintext in `home.tar`, a wrong key failing without
  consuming the archive, and the restored token; it runs in CI's Docker
  job, not locally (the Docker engine was down after the disk filled up).
- **Item 4 (Codex sign-in):** `auth.json` in the secret store, a private temp
  `CODEX_HOME` per app-server run. Tested with a fake `codex`: sign-in and
  refresh stored back, the home is 0700 and `auth.json` 0600 while Codex
  runs, no `auth.json` left on disk afterwards, an old sign-in moves over,
  deleting the provider deletes it. Not tested with the real Codex CLI
  (assumes it writes `auth.json` before `account/login/completed`).

- **Item 5 (connector OAuth):** the DCR client secret is kept in the
  keychain; revocation on sign-out and delete came with the OAuth apps PR.
  Tested with a fake OAuth server that registers confidential clients
  (`connectors_keep_the_secret_a_server_registered_them_with`: sign-in, then
  a refresh in a fresh session with the stored secret, registration stored
  only after the sign-in and forgotten on sign-out) and in
  `connectors_use_an_own_oauth_app_refresh_and_revoke` (no revocation over
  http; https revocation covered by unit tests only, no real server).

## Connector OAuth apps, review fixes (`feature/connector-oauth-clients`)

**Done**

- Own OAuth app per connector (client id, secret, scopes, issuer), Google
  offline access, `expired` sign-ins, revocation on sign-out (RFC 7009) and
  on delete, Settings page, model row in the profile.
- Review findings fixed: issuer binding against AS mix-up; revocation checks
  the issuer and stays out of the LAN; secret dropped on URL/client
  id/issuer change; scope cap checked as it fills (was O(n²) before the cap);
  sign-ins started before a change are discarded; no duplicate Google
  parameters.

**Tested**

- `connectors_use_an_own_oauth_app_refresh_and_revoke`: sign-in with the own
  app, refresh with the secret, rejected refresh shows `expired`, sign-out,
  no revocation over http, missing or http issuer refused, 40 scopes refused,
  a server whose issuer is not the app's gets no sign-in, a new client id
  drops the secret. Unit tests: issuer check, internal addresses, a
  revocation URL on 127.0.0.1 refused unless the connector is local, Google
  parameters.

**Not tested**

- Revocation against a real https server; Google itself (needs Tim's
  Workspace preview and OAuth client); the Settings page in the Tauri window.

## Live smoke test (hardening pass, 2026-11-20)

- **Runner image and Docker e2e: ran in CI only.** On every hardening PR
  (#6 to #12) the "E2E (Docker backend)" job built `runner/Dockerfile` and
  ran the sandbox Docker test (now with encrypted archives: header on disk,
  no plaintext, wrong key refused, token from the container env) and the
  daemon's Docker e2e; all green. Not run on Tim's Mac: Docker Desktop
  stopped answering after the disk filled up and did not come back after a
  restart.
- **Real Claude turn: not run.** `ANTHROPIC_API_KEY` is not in this
  session's environment, and the key must not go through the transcript.
  `scripts/smoke-live.sh` runs it from Tim's own terminal.
- **Ollama turn: not run.** Ollama is installed but was not running and has
  no model pulled; the script needs `OLLAMA_MODEL` (default `qwen2.5:3b`).
- **Key leak check: written, not yet run live.** `smoke-live.sh` counts
  matches of the real key over the whole stream of: container config and
  env, runtime env, root filesystem, home volume, container logs, daemon log
  and data dir; a source that cannot be read fails the check. Self-tested
  against known inputs (found, absent, unreadable, key at the end of a large
  stream).
