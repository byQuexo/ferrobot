# Decisions

Open questions from `docs/roadmap.md` that were settled during implementation,
with the assumption made. Newest phase last. Revisit any of them by editing
the code and this file together.

## Process

- **One branch per phase, stacked.** Nothing is merged, so each phase branches
  off the previous one: `feature/m1-storage` → `feature/providers` →
  `feature/codex-driver` → `feature/sleep-levels` → `feature/firecracker-client`.
  Merge them in that order.

## M1 Storage

- **Where the code lives.** `ferrobot-sandbox` depends on `object_store`
  without features (only the `ObjectStore` trait); the daemon builds the
  store from config (`crates/daemon/src/storage.rs`) and is the only crate
  with the `fs` and `aws` features. The plain core crates stay lean.
- **No wrapper type.** `object_store`'s own `ObjectStore` trait is the API;
  ferrobot adds only `storage::open(url, data_dir)`.
- **Config is one URL:** `--storage-url` / `FERROBOT_STORAGE_URL`, e.g.
  `s3://bucket/prefix`. Credentials, region and endpoint come from the
  standard `AWS_*` variables of the daemon (R2: `AWS_ENDPOINT` +
  `AWS_REGION=auto`). They are read by the daemon only; sandboxes get an
  explicit env and never inherit them.
- **Default location is the data dir itself,** with keys
  `snapshots/<tag>.tar`. That is exactly where older releases wrote snapshot
  archives (`<data dir>/snapshots/`), so existing snapshots keep restoring
  without a migration.
- **S3 support is always compiled in** (`aws` feature), not behind a cargo
  feature: switching to S3/R2 stays a config change.
- **Local backend snapshots are unchanged** (directory copies under
  `sandboxes/.snapshots`); the roadmap only moves Docker snapshot tars.

## Phase 2: Providers and drivers

- **Credentials at rest:** `keyring` 4.2 (MIT/Apache-2.0) with its default
  native stores. `--secret-store file` (plaintext `<data dir>/secrets.json`,
  mode 0600, loud warning) exists for headless CI only. Without a usable
  keychain the daemon still starts and warns; storing a key then fails with a
  500 instead of falling back to plaintext.
- **`--driver`** keeps its values: `mock` forces the mock driver for every
  agent (tests, demos); `claude` now means "real models": each agent's
  provider decides the driver, agents without one use Claude with
  `ANTHROPIC_API_KEY`. A missing `ANTHROPIC_API_KEY` is a warning, not a
  startup error, so OpenAI-only setups work.
- **Two drivers cover eight provider kinds.** Anthropic, Bedrock and Vertex use
  the Claude Agent SDK (its own env switches); OpenAI, Gemini (its
  OpenAI-compatible endpoint), OpenRouter, Ollama and generic endpoints use a
  new OpenAI-compatible runner driver. No new npm dependency.
- **Bedrock without SigV4.** Bedrock providers need a Bedrock API key. The
  sandbox gets `AWS_BEARER_TOKEN_BEDROCK=<agent token>`, the proxy swaps in the
  Bedrock key. IAM credentials / SigV4 signing are not supported. Checked by
  capturing the bundled Claude binary's requests against a local server: it
  sends `Authorization: Bearer` to `<base>/model/<id>/invoke` and calls
  `inference-profiles` (routed to the Bedrock control plane).
- **Vertex via the host's gcloud.** The proxy runs
  `gcloud auth print-access-token` on the daemon host and caches the token
  for 45 minutes. With `CLAUDE_CODE_SKIP_VERTEX_AUTH=1` the SDK sends the
  agent token as `x-api-key` (also checked by capture), which the proxy
  replaces. No service-account key handling in ferrobot.
- **The proxy authenticates before it reveals configuration** (unknown token:
  401 even when no key is configured). Upstream is always taken from the
  provider config, never from the request.
- **Changing an agent's provider or model rebuilds its sandbox** (disk kept),
  because the driver, model and cloud settings are fixed in the container env.
  The same happens to all agents of a provider when its region, project or
  default model change. Key changes need no restart (the proxy reads the key
  per request).
- **Agents created by agents inherit** the creator's provider and model.
- **Deleting a provider in use is refused** (409); the agents have to switch
  first.
- **Subscription logins (Claude OAuth) are not built**, per Tim's rule.

## Phase 3: Codex driver

- **Codex runs inside the sandbox.** The runner spawns `codex app-server`
  (JSON lines over stdio) so Codex's shell and patch tools act on the agent's
  own computer. A host-side app-server would have run agent commands on the
  daemon host.
- **No credential in the sandbox.** The runner writes `$CODEX_HOME/config.toml`
  with a custom model provider (`base_url` = the daemon proxy, `env_key` =
  `FERROBOT_AGENT_TOKEN`, Responses wire API) and the ferrobot MCP server with
  the same bearer variable. Only the variable *name* is written.
- **New provider kind `codex`** (driver `codex`). With an API key the proxy
  forwards `/responses` to `https://api.openai.com/v1`. Without one it uses the
  host's ChatGPT sign-in: upstream `https://chatgpt.com/backend-api/codex`,
  `Authorization: Bearer <access token>`, `chatgpt-account-id: <account>`
  (header name from Codex's `openai_headers.rs`); inbound
  `chatgpt-account-id` headers from agents are dropped.
- **ChatGPT sign-in on the host via the official surface.** The daemon runs the
  host's `codex app-server` with `CODEX_HOME=<data dir>/codex/<provider id>`
  and calls `account/login/start {type: "chatgptDeviceCode"}`; the UI shows the
  URL and code. Tokens are read with `getAuthStatus {includeToken,
  refreshToken}` (Codex refreshes them itself) and cached until 5 minutes
  before the JWT's `exp` (at most 30 minutes). The account id comes from the
  token's `https://api.openai.com/auth.chatgpt_account_id` claim.
- **Not used:** `account/login/start {type: "chatgptAuthTokens"}`, which would
  hand tokens to an app-server; Codex marks it "OPENAI INTERNAL USE ONLY".
- **Approvals:** `approval_policy = "untrusted"` so Codex asks before commands
  and patches; the runner answers with the same `decide()` policy as the other
  drivers. `sandbox_mode = "danger-full-access"`: the container is the
  boundary, Codex's own Landlock/Seatbelt sandbox is not relied on. ferrobot
  MCP tools are pre-approved (`default_tools_approval_mode = "approve"`).
- **Open question for Tim (not decided here):** whether to add `@openai/codex`
  to the runner image (a new npm dependency) and whether proxying a ChatGPT
  sign-in this way fits OpenAI's terms for third-party tools. Until then the
  driver fails with "codex is not installed in this sandbox image".

## Phase 4: Warm and cold sleep

- **`SleepLevel { Warm, Cold }` in the trait** via a new `sleep(handle, level)`
  method that returns the level reached. `Cold` defaults to `stop`; `Warm`
  defaults to `NotImplemented`, so backends that cannot pause are not stopped
  early: the supervisor keeps such agents awake until the cold timeout, and
  `POST /v1/agents/{id}/stop?level=warm` answers 501 there.
- **Docker Warm = `docker pause`**, wake = `unpause`; `stop` unpauses first so
  the runner gets SIGTERM. A container that is not running cannot be paused
  and reports `Cold`.
- **New `AgentStatus::Paused` and `SandboxState::Paused`.** Paused agents are
  skipped by crash recovery (the roadmap's "Warm is not a crash" pitfall);
  they only go colder (`Paused → Stopped` after `--idle-timeout`). Every path
  that delivers work (`ensure_awake`: messages, routines, exec) resumes them.
  No migration: `paused` is a new value of the existing text column; an
  older daemon would fail to read such a row.
- **Defaults:** `--warm-after 300` (`FERROBOT_WARM_AFTER`), cold stays
  `--idle-timeout 900`. The roadmap's `FERROBOT_COLD_AFTER` is the existing
  `FERROBOT_IDLE_TIMEOUT`, not a new name.
- **WebSocket during a pause:** there is no keepalive on either side, so the
  runner's connection survives; a long pause is untested.
- **Presence** shows paused agents as sleeping; the info panel says "paused,
  resumes quickly".

## Phase 5: Firecracker without KVM

- **Thin client, no SDK.** HTTP/1.1 over the API socket with `hyper` 1 +
  `hyper-util` + `http-body-util`, which were already in `Cargo.lock`
  transitively (no new packages). Not `firecracker-rs-sdk`, not
  `firecracker-containerd`.
- **Runner env through MMDS (V2)**, never kernel args or disk: the token is
  in guest memory only, like in a container's env.
- **One shared bridge** (`fcbr0`, host `172.16.0.1/24`, guests `.2`, `.3`, …,
  TAP `fctap<N>`, index kept per VM so the IP is stable). The sandbox daemon
  URL is `http://<fc-host-ip>:<agent port>`; the agent API must listen on the
  bridge IP or `0.0.0.0`. `scripts/firecracker/README.md` lists firewall rules
  so guests reach only the agent port.
- **Home disk:** a sparse file on the host; the guest init formats it on
  first boot only (`blkid -p` exit 2). No `mkfs` on the host.
- **Snapshot support is a backend property** (`supports_snapshots()`),
  replacing the hardcoded `kind == "docker"` in the sandbox info API.
- **Warm = `PATCH /vm Paused`**, Cold = stop for now (snapshot sleep is F4b).
- **Left for Tim (needs a decision on a KVM host):** stopping the daemon
  kills all VMs (`kill_on_drop`); aarch64 has no `SendCtrlAltDel`, so `stop`
  there is a hard kill; `destroy(false)` leaves the VM `Stopped`, not
  `Missing`; the guest has no DNS yet.

## Live smoke follow-ups

- **Codex is in the runner image, pinned to 0.166.0** (`@openai/codex`,
  Apache-2.0, ~27M weekly downloads; Tim asked for it). Pinned to a release
  about two weeks old rather than the newest one.
- **Codex approvals: `on-request`.** 0.166 refuses `untrusted`. With
  `danger-full-access` Codex rarely asks, so the runner's `decide()` guard
  rails mostly do not apply to Codex's own commands; the container stays the
  boundary. Requests Codex does send are still answered with `decide()`.
