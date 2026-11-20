# Progress

Status per roadmap phase (`docs/roadmap.md`). Decisions are in
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
  stream); Claude SDK, Codex and the runner's own client work the same.

**Open**

- No real Gemini model (no `GEMINI_API_KEY` in the environment).
- Gemini 0.67 runs some commands without asking, so `decide()` only sees what
  it asks about; its shell results reach the timeline as "completed" only.
- The `session/load` replay drain is timing-based (200 ms quiet, max 5 s).
