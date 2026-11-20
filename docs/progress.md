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
- **Live smoke test passed (2026-11-20, third run, `SKIP_BUILD=1`).** Tim
  stored his Anthropic key through Settings (Keychain); the script got it from
  the Keychain with `security` (never printed) and gave it to a throwaway
  daemon as a new Anthropic provider. Results: provider reports the key, key
  is in the macOS Keychain; Claude turn `completed` with a tool call, the
  nonce file in the sandbox and the nonce in its answer; `qwen2.5:3b` on
  Ollama passed as well this time; the real key was in none of: both
  containers' config/env, runtime env, root filesystem, home volume and
  logs, the daemon log, the data dir. Not checked in that run: that deleting
  the provider removes its Keychain entry (the check misread `security`'s
  exit code 44 and printed nothing; fixed afterwards, not re-run).
- **Ollama turn: run twice on Tim's Mac (2026-11-20, M2 Air, 16 GB, Ollama
  0.40.16, `OLLAMA_CONTEXT_LENGTH=32768`), Claude part skipped (no key yet).**
  The path works: a Docker sandbox, the OpenAI-compatible driver, the
  daemon's model proxy to local Ollama, a shell tool call, the nonce file in
  the sandbox, clean-up (no containers or volumes left). Both runs still FAIL
  the script's answer check, which is the models' limit, not ferrobot's:
  - `qwen2.5:3b`: turn `completed`, but it called `send_message` to an agent
    "user" (`agent not found: user`) and answered without the file's content.
  - `qwen3:4b`: wrote the file, then thought at about 23 tokens/s for longer
    than the script's 180 s, so no `turn_done` arrived in time.
  For real agent work on this machine use a cloud model (see the Ollama
  notes); a local model this size is only good for checking the plumbing.
- **Key leak check: ran live in the passing run above.** `smoke-live.sh` counts
  matches of the real key over the whole stream of: container config and
  env, runtime env, root filesystem, home volume, container logs, daemon log
  and data dir; a source that cannot be read fails the check. Self-tested
  against known inputs (found, absent, unreadable, key at the end of a large
  stream).
- **The Claude turn goes through the OS keychain:** the script asks for the
  key (hidden, not in the shell history), creates an Anthropic provider over
  stdin, checks the key is in the keychain (macOS: `security`), runs the
  agent on it with no `ANTHROPIC_API_KEY` in the daemon's environment, and
  checks that deleting the provider removes the keychain entry. That is the
  first run of the real keychain path; Windows (Credential Manager) still
  has none, the script is bash.
- **What the script asserts per agent:** a completed `turn_done` whose result
  contains a fresh nonce, a `tool_use` event and the nonce in the sandbox
  file; it rebuilds the runner image first and reads the data dir only
  after the daemon stopped.

## Agent collaboration (`feat/agent-collaboration`)

- **Done:** mail from an agent wakes a sleeping one (now tested); tracked
  tasks (`assign_task`, `create_agent` with `task`, `list_tasks`) with an
  automatic report when the delegate's turn ends and one reminder from the
  supervisor; memorable names asked for in the prompt and `create_agent`;
  a greeting to the operator at the start of a new agent's first turn, in a
  turn of its own for operator-created agents. Design in `docs/decisions.md`.
  No migration; the runner protocol only documents `message_id: 0`.
- **Tested** (in-process daemon, fake runner frames, real MCP tools):
  `agent_mail_wakes_sleeping_agents` (paused, stopped and archived agents
  woken by `send_message`, tool descriptions), `tasks_report_back_once_and_never_loop`
  (report without a reply, none when the delegate replied, one reminder
  only, a reply threaded to the reminder counts, `list_tasks`, the
  assigner's turns on answers send nothing back),
  `new_agents_greet_the_operator_first` (greeting turn, greeting before a
  queued message, greets once, no own turn for agent-created agents). Runner:
  the prompt lines (`systemAppend`). Live, with the real `ferrobotd`, the
  local backend and the mock driver: an operator-created agent's greeting
  turn runs, shows as its first chat text and counts as unread; its next
  turn has no greeting.
- **Not tested:** a real model with any of it: whether models end task turns
  with their answer, keep `reply_to` for the final answer, pick memorable
  names and greet in one or two lines. `smoke-docker.sh` was not run (it
  binds 7878/7879). Not handled: a delegate that hands part of its task to a
  third agent gets that agent's answer as a normal message, so its final
  answer to the first assigner then depends on the model again.
## Release prep 0: conventions and cleanup (`chore/conventions`)

**Done**

- `docs/CONVENTIONS.md`, linked from CLAUDE.md. Enforced by `.editorconfig`,
  `clippy.toml` and the workspace lints `unwrap_used`, `expect_used` and
  `panic` (allowed in tests and at two annotated startup sites), and by
  Prettier in the runner (`npm run format:check`, also in CI).
- Comments trimmed to the why in all crates, the runner, scripts and the Tauri
  shell; wrong doc comments fixed (CLI help of `--listen`, `--warm-after`,
  `--codex-bin`, `--model`; `update_agent_profile`; `new_token`; the
  permission policy's scope). Each `TODO(kvm)` points to an issue draft in
  `docs/launch/issues/`.
- Promise chains in the runner became `async`/`await`.
- `scripts/smoke-docker.sh` and `smoke-live.sh` take their ports from
  `FERROBOT_SMOKE_PORT` / `FERROBOT_SMOKE_AGENT_PORT`; the Docker smoke test
  uses the file secret store (no keychain prompt).
- Test diet: Rust 134 → 133 (two near-identical pairs merged into table tests,
  one test added for the timestamp fix), runner 83 → 80 (four mock-parser
  tests merged into one table with all eight cases), desktop 17. No security
  test was removed.

**Bugs fixed on the way**

- Stored timestamps had a variable width (RFC 3339 drops a zero fraction), so
  SQLite's text comparison put `…:00Z` after `…:00.5Z`: a routine due on a
  whole second could be missed for up to a second while the scheduler spun.
  Timestamps are now fixed-width microseconds; old rows sort at most one
  second off until they are rewritten.
- The runner's shell guard let `rm -rf /*`, `rm -rf ~/`, `rm -r -f /` and
  `rm -rf --no-preserve-root /` through.
- The ACP driver allowed edit, delete and move requests that named no path.
- The desktop shell injected the operator token into any page on `localhost`;
  now only the Vite dev server port (1420) and the app's own pages get it.

**Not done here**

- Files the open voice PR changes (`state.rs` apart from one attribute,
  `runner_ws.rs`, `api/mod.rs`, `error.rs`, `spec.rs`, `api/providers.rs`,
  `lib.rs`, `features.rs`, `specs.rs`, `mcp` `lib.rs`/`tools.rs`, store
  `lib.rs`/`migrations.rs`), API doc comments that generate `protocol/`, and
  the desktop UI (ESLint, Prettier, comments): after that PR merges.
- Found, not fixed: on Firecracker an archive request stops the VM and
  reports success, so `--archive-after` would stop it again every tick; a
  message steered into a Claude turn whose resume then fails is reported as
  seen but never reaches the model; the bearer scheme is matched
  case-sensitively (all our clients send `Bearer`).

## Release prep 1: security leftovers (`fix/security-leftovers`)

**Done**

- Archived and snapshot images blank every env variable that holds the agent
  token (value match), so `ANTHROPIC_API_KEY`, `GEMINI_API_KEY` and
  `AWS_BEARER_TOKEN_BEDROCK` no longer keep the old token in the image config.
- CI job "Dependency audit": `cargo audit` (workspace and Tauri lockfiles) and
  `npm audit` (runner, desktop). Local result: no vulnerability after the
  `time` update; unmaintained-crate warnings listed in decisions.
- The shared-page CSP test also asserts that `allow-same-origin` is absent.

**Tested**

- Unit test of the env change; the Docker sandbox test now gives the sandbox
  the token under all four names and checks the snapshot image and the image
  restored from an archive carry no env value with the token (Docker Desktop
  on macOS, `ferrobot-runner:dev` image).
## Release prep 3: contributor docs (`chore/contributor-docs`)

**Done**

- README: pitch, download section (unsigned-build steps for macOS and
  Windows), a five-minute quick start with Docker Desktop and Ollama and no
  key, a new architecture diagram with the model proxy, "Status and limits";
  stale lines fixed (Codex is in the image, a live Claude turn ran, archives
  are encrypted, which routes are open, 244-bit agent tokens).
- CONTRIBUTING.md, CODE_OF_CONDUCT.md (Contributor Covenant 2.1), SECURITY.md
  (private advisories, known open hardening items), issue forms, PR template,
  CODEOWNERS, ten issue drafts in `docs/launch/issues/`.
- Clean-room and employer check: no OpenBot or Grok Bot material; the only
  mentions are the README's clean-room note and CLAUDE.md's rules. No
  employer named anywhere.

**Not verified**

- The README describes the desktop download as section 7 builds it (bundled
  daemon, Docker check, runner image on ghcr.io); true once that lands.
- The Code of Conduct points reporters to the email on the maintainer's
  GitHub profile; GitHub has no private messages.

## Release prep 5: wiki (`chore/wiki`)

**Done**

- `docs/wiki/` as GitHub-wiki pages: Home, sidebar, Architecture (mermaid),
  Roadmap (M1, L0–L5, F0–F5 with status, no dates), Getting Started (desktop
  and source paths), Providers, Sandbox Backends, Usage and Costs,
  Conventions, FAQ.
- `scripts/publish-wiki.sh` copies the pages into the `.wiki.git` repository
  over SSH; dry run unless `--yes`.

**Not verified**

- The script never ran (the wiki needs a public repo or a paid plan, and a
  first page created on GitHub before its git repository exists); checked
  with `bash -n` only.

## Release prep 8: repository settings (`chore/github-settings`)

**Done**

- `docs/launch/github-settings.md` and `scripts/apply-github-settings.sh`
  (dry run unless `--yes`, refuses on a private repo or a
  `pull_request_target` workflow that touches PR code): ruleset for `main`
  (no deletion or force push, PR with one approval, stale approvals
  dismissed, code owner review, every PR job required, linear history, admin
  bypass for a solo maintainer), approval for outside contributors' workflow
  runs, read-only default token.
- CI: a `Changes` job replaces `paths-ignore`, so docs-only PRs skip the real
  jobs instead of leaving required checks pending forever.

**Not verified**

- Nothing ran against GitHub. `actor_id 5` (admin role) and the check names
  as ruleset contexts are taken from docs, not tried. Linear history means
  switching from merge commits to squash merges before the ruleset goes on.
- The new `Changes` job has not run on GitHub (Actions minutes are used up).

## Release prep 7: desktop release (`feat/desktop-release`)

**Done**

- The desktop shell starts the bundled `ferrobotd` and stops it on exit; the
  daemon keeps running without Docker and says so in `/v1/health`; the app
  then shows a "Docker is not running" page that continues by itself.
- Docker pulls the runner image on first use; release builds name
  `ghcr.io/byquexo/ferrobot-runner:0.1.0`.
- `.github/workflows/release.yml` (tag `v*`): draft release, universal `.dmg`,
  NSIS `.exe`, `SHA256SUMS`, multi-arch runner image; optional signing.
- `CHANGELOG.md`; version 0.1.0 was already set everywhere (workspace,
  `package.json` files, `tauri.conf.json`, the Tauri crate).

**Tested**

- Daemon test: `/v1/health` reports `sandbox_ready: true` with a working
  backend. All Rust, runner, desktop and Tauri shell gates pass.
- `ferrobot_0.1.0_aarch64.dmg` built locally (`CI=true` skips the Finder
  layout step, which times out without a GUI session; GitHub runners set
  `CI`). The bundle holds `ferrobotd` next to the app binary, and
  `ferrobotd --version` runs from the mounted image.
- The release `ferrobotd`, started the way the app starts it but on spare
  ports with the mock driver and the file secret store: health reports
  `sandbox_ready: true`, a mock agent in a Docker container answered, and
  deleting it left no container.
- Without a Docker socket the daemon starts and reports `sandbox_ready:
  false`; once the socket appears, the next health call reports `true`.
- The Docker sandbox test passes with the reconnecting client.

**Not verified**

- The app was not launched: the window, the sidecar start and stop, the
  Docker page, the token injection and the blob download handler. A launch
  needs port 7878 (in use on the build machine) and the real keychain; left
  for Tim.
- The universal macOS build, any Windows build (Windows: CI only, never run
  on real hardware), signing, notarization.
- The release workflow and the runner image push never ran (no Actions
  minutes; macOS runners cost ten times the minutes on a private repo and are
  free once the repo is public). `ubuntu-24.04-arm` runners are free for
  public repos only.

## Model picker (`feat/model-picker`)

**Done**

- `GET /v1/providers/{id}/models` (the provider's list, or suggestions) and
  `GET /v1/provider-kinds/{kind}/models` (suggestions only); spec and desktop
  types regenerated.
- A model dropdown with "Custom…" for the provider's default model (add
  form) and the agent's model (new agent, edit, and the profile's Change,
  which opens the same dialog).

**Tested**

- Parser table test, one fixture per list format (Anthropic, OpenAI,
  OpenRouter, Ollama, Gemini).
- Feature test against a fake upstream: the Anthropic key arrives as
  `x-api-key` with `anthropic-version` and `limit=1000`, the OpenAI key as a
  bearer, no key in any response; a wrong key and a closed port fall back to
  suggestions; failures are not cached; an edit drops the cache; the kind
  route, and 404 for an unknown kind.
- Desktop unit test for the picker's options (`modelOptions`).
- In a browser against a real `ferrobotd` (temp data dir, file secret store,
  a fake list server): the live list for an OpenAI-compatible provider,
  suggestions for an unreachable one, "Custom…" focuses a text field, "List"
  keeps the typed id; the add form switches lists with the kind.
- Gates: fmt, clippy `-D warnings`, `cargo test --workspace`, core check,
  specs; desktop `check:api`, typecheck, test, build. Runner untouched.

**Not tested**

- No real provider list was fetched (no keys): Anthropic, OpenAI, OpenRouter,
  Ollama, Gemini (does its list carry `models/`?) and Codex with a key need a
  live check.
- The Bedrock `global.anthropic.claude-*` ids on `bedrock-runtime`, and
  whether the runner's Codex and Gemini CLI accept the suggested GPT-6 and
  Gemini 4 ids.
- The Tauri window (browser only); the Tauri shell gate (no change there).

## UI polish (`feat/ui-polish`)

- An unslop polish pass over the desktop UI (direction and tokens in
  `decisions.md`): 4 px grid, type 12/13/14/16/20 at 400/500, lucide at
  16/18 with a 1.5 stroke, sentence-case labels. Settings: a 640 px column,
  status-dot rows, add forms behind a button, hairline sections, 36 px nav
  rows, the switch in the accent. Voice copy names the phone button. The
  five info tabs fit on one row, and the theme control no longer overlaps.

**Checked visually**

- Chat, agent list and Settings (providers, connectors, voice, appearance)
  in light and dark, in Chrome against a throwaway `ferrobotd` (local
  backend, mock driver, file secret store); before and after shots in
  `docs/assets/ui-polish/`.
- Gates: desktop `check:api`, typecheck, test, build.

**Not checked**

- The Tauri window (browser only).
- Tool cards, question cards, agent-to-agent mail and the dialogs (agent,
  file viewer): the mock driver produces none, so they were not rendered.
- The model list in the add-provider form: the daemon binary was built from
  `feature/voice`, before the model routes, so the field fell back to text.
- A stored voice key with a real catalog.

## Release prep 6: usage and cost (`feat/usage`)

**Done**

- Migration v13: table `usage`, one row per model call a known agent makes
  through the proxy (agent name kept, no FK, so rows outlive the agent).
- The proxy meters generation calls while the body streams past: Anthropic
  Messages (also Vertex `rawPredict`/`streamRawPredict` and Bedrock
  `invoke`), OpenAI-style `chat/completions`, Codex `responses`, Gemini
  `generateContent`. Rows also for refused calls (`refused`) and upstream
  errors (`error`). No prompt or completion is stored.
- The OpenAI-compatible driver asks for `stream_options.include_usage`.
- `config/pricing.toml` (Anthropic, OpenAI, Gemini; checked 2026-11-20 on
  their pricing pages), operator overrides in the `pricing` setting.
- `GET /v1/usage` (filters, group by day/agent/provider/model, estimated
  cost), `GET`/`PUT /v1/usage/prices`.
- Desktop: a Usage view (chart icon next to Settings) with ranges, totals,
  a cost-per-day chart, sortable tables by provider, model and agent, CSV
  export and a price edit per model row.
- Follow-up (`feat/usage-chart`): the chart stacks each day by the chosen
  breakdown (the biggest five, or four plus "Other"), falls back to tokens
  when no cost is known, and counts days in the daemon's `--timezone`
  (`GET /v1/usage?daily=true`, new `days` and `timezone` fields).

**Tested**

- Daemon: Anthropic SSE with cache tokens and OpenAI SSE with
  `include_usage` against a fake upstream, split mid-line; a refused call;
  filters, groups, unknown and not-tracked counts, overrides; usage parsing
  per API family; cost math; the price file; the v12 to v13 upgrade.
- Runner: the request carries `stream_options`, a usage-only last chunk
  does not break the turn. Desktop: range math, UTC days, sorting, CSV.
- The view was checked in a browser (`vite preview`) against a real daemon
  with seeded rows: totals, chart, tables, price edit.
- Live, Ollama only (2026-11-20, after rebasing on main): a real daemon in
  Docker with a runner image built from this branch, an Ollama provider,
  `qwen2.5:3b`, one tool-using turn: two usage rows with input, output and
  cache-read tokens from Ollama's own usage fields, cost 0.

**Not tested**

- No cloud upstream: no Anthropic, OpenAI, Gemini or Codex response has
  been metered. `scripts/smoke-live.sh` was not re-run after the usage/proxy
  changes (Tim's call, 2026-11-20).
- After the review fixes, the same Ollama run recorded four `ok` rows (the
  runner stops reading at `[DONE]`, which now counts as the end).
- Gemini native and Codex Responses only against unit fixtures; whether
  strict OpenAI-compatible servers accept `stream_options`.
- CSV export in the Tauri window: the shell's download handler saves blob
  downloads to Downloads (release prep 7), never tried in the window.

## Provider prompt caching (`feat/openrouter-cache`)

- OpenRouter: top-level `cache_control` for Claude chat calls and
  `x-session-id` per agent, added in the proxy. Tested with a fake
  upstream (Claude gets the marker and the session, other models only the
  session, other providers neither) and a unit test (an existing
  `cache_control` and non-JSON bodies stay untouched). Not tested against
  the real OpenRouter (no key used): whether cached tokens show up in its
  usage as documented.


## Release prep 0, second part: desktop and deferred files (`chore/desktop-lint`)

**Done**

- Desktop: ESLint (`strictTypeChecked`, named exports only) and Prettier for
  TS files (`npm run lint`, `npm run format:check`, both in CI). The
  formatting is its own commit. Lint findings fixed: a dynamic `delete`
  replaced; the deprecated ScriptProcessor calls in `voice.ts` keep an
  `eslint-disable` with the CSP reason.
- Comments trimmed in the desktop and in the files the earlier pass left for
  the voice and collaboration PRs; API doc comments corrected (runner
  `driver` values, `Presence::Sleeping`, `StopQuery.level`, `page_url`,
  "secret store" instead of "keychain", and more), so `protocol/` and the
  desktop types are regenerated.
- CI's npm audit fails on moderate advisories in shipped dependencies and on
  high ones in dev tooling. The desktop's lint tooling carries a moderate
  `minimatch` advisory whose fixed release is newer than this machine's npm
  release-age cutoff; it clears with the next update.
- Tests: desktop 30 → 29 (one model-picker case folded into a table), Rust
  and runner unchanged.

**Found, not fixed (reported)**

- Desktop: the provider re-key, connector token and OAuth-app forms and the
  snapshot label close even when the save failed, so typed input is lost; a
  live-transcription socket that drops after opening is not reported; an
  unknown routine shows as `#<id>`; ChatGPT sign-in polls without a limit;
  sorting usage ascending puts unknown costs first.
- Daemon: message bodies are length-checked before `@name` becomes
  `<@uuid>`, so a stored body can exceed the cap by a few bytes per mention.

## Release prep 4: demo GIF (`docs/demo-gif`)

**Done**

- `docs/assets/demo.gif` (1.0 MB, 960 px, 12 fps) and `demo.mp4` (0.4 MB),
  about 17 s, embedded in the README: two agents created, one messages the
  other (`@bob …`, shown as an agent-to-agent hint), a question card is
  answered, the agent replies, then the Usage view.
- Recorded on the polished UI (Vite dev server on 1420) against a throwaway
  `ferrobotd` on spare ports (Docker backend, `--secret-store file`, temp
  data dir, mock driver). The usage rows come from one real turn of an
  Ollama agent (`qwen2.5:3b`) run before the mock restart, so the cost is
  $0.00; no key or personal data is on screen.

**How**

- The make-demo plugin was not installed (`/plugin` is interactive). The
  installed Playwright (from the global `@playwright/cli`) drove its cached
  Chromium; its own video recorder needs an ffmpeg build that is not
  installed, so frames came from the CDP screencast and the system ffmpeg
  encoded them.

## Release prep 7, follow-up: Windows console and first pull (`fix/windows-console`)

- The desktop shell starts `ferrobotd` with `CREATE_NO_WINDOW` on Windows, so
  no console window opens next to the app (closing it would kill the
  daemon). Compiled only on Windows: unverified, like every Windows build.
- Checked the first-run pull with a small image that was not local
  (`busybox:1.36.1`, spare ports, file secret store): the daemon logged the
  pull, the image arrived, and `POST /v1/agents` returned only after the pull
  (7 s here). With the 2 GB runner image the first agent's create call blocks
  for minutes; noted under the README's limits.
- CORS already allows both Tauri origins (`tauri://localhost`,
  `http://tauri.localhost`); `/v1/ws` checks no origin.

## Fixed after the lint review (`fix/ui-and-limits`)

**Done**

- Desktop: the provider re-key, connector token and OAuth-app forms and the
  snapshot label close only after the save succeeded; on failure the input
  stays and the error shows as a toast.
- Desktop: a live-transcription socket that closes on its own (or a provider
  error) releases the microphone and is reported once. A call stops listening
  but keeps reading replies aloud; dictation ends as if stopped and keeps the
  text in the box. A provider error now ends the session as well (its toast
  already said "stopped" while audio kept streaming).
- Desktop: a message from an unknown or deleted routine shows "A routine"
  instead of `#<id>`.
- Desktop: the ChatGPT sign-in stops polling after 15 minutes (the daemon's
  `LOGIN_TIMEOUT`) and says it timed out.
- Desktop: unknown costs sort last in the Usage table in both directions.
- Daemon: chat, call and the MCP `send_message`, `assign_task` and
  `create_agent` task bodies are length-checked after `@name` becomes
  `<@uuid>`, so mentions can no longer push a stored body past the limit
  (still 400 / invalid params). The `PostMessage` and `CallMessage` docs say
  so; `protocol/` and the desktop types are regenerated.
- Daemon: `create_agent` checks its task as stored (mentions of the new
  agent included) before the approval card, so a too-long task creates no
  agent.
- Daemon: a call's heard lines are capped with their joining newlines and
  the intro texts counted, so an assembled call body stays within 32 KiB.

**Tested**

- Desktop unit tests: an unknown routine in `buildTimeline`, the ascending
  cost sort. Rust: `message_limits_count_mentions_as_stored` (chat, call,
  `send_message`, `assign_task` with a body at the limit that grows through a
  mention, and a `create_agent` task that mentions the new agent). The chat,
  call, `send_message` and `create_agent` legs each failed with their fix
  reverted; `assign_task` goes through the same check in `enqueue`.
  `call::tests::many_tiny_lines_keep_the_call_body_within_the_limit` (1000
  one-letter lines) failed with the old cap.
- Not tested: the form, live-socket and sign-in timeout changes (no component
  tests; not run in a browser or the Tauri window).

## Auto mode (`feat/auto-mode`)

**Done**

- `GET`/`PUT /v1/settings` with `auto_mode` (default off), stored as the
  `general` setting; no migration (the `settings` table exists since v12).
- With Auto mode on, `create_agent` creates the agent without an approval
  card and records an `agent_created` notice in the creator's top chat; the
  agent limit, name and task length checks still apply.
- `create_agent` description and the Claude driver prompt say the operator
  approves unless Auto mode is on.
- Desktop: Settings, Agents with an "Auto mode" switch (saved on toggle);
  the timeline shows the notice. `protocol/` and the desktop types are
  regenerated.

**Tested**

- Rust: `auto_mode_creates_agents_without_a_card` (setting round-trip, no
  `question` event, notices in the top chat for a direct and a sub-agent
  creation, nothing created over the limit). The approval tests stay green.
- Desktop unit test: the `agent_created` notice in `buildTimeline`.
- Not tested: the Settings switch in a browser or the Tauri window; Auto mode
  with a real model calling `create_agent`.
