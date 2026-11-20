# Decisions

Open questions from the roadmap that were settled during implementation,
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
  host's `codex app-server` with a private `CODEX_HOME` (see hardening item 4)
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

## L4: ACP driver

- **One generic ACP client, one agent for now** (`FERROBOT_ACP_AGENT=gemini`).
  Gemini specifics: `--acp --model`, `GEMINI_CLI_TRUST_WORKSPACE=true`, and
  `security.auth.selectedType = "gemini-api-key"` merged into
  `~/.gemini/settings.json` (without it Gemini picks its gateway auth and
  refuses headless). No secret is written there.
- **New provider kind `gemini_cli`** instead of reusing `gemini`: `gemini`
  stays the OpenAI-compatible API path, `gemini_cli` is the harness with its
  own tools.
- **Instructions are prepended to the first prompt** of a session: Gemini
  does not offer ACP's system-prompt capability.
- **`@google/gemini-cli` pinned to 0.67.0** (Apache-2.0, ~500k weekly
  downloads), about two weeks old; Tim asked for Gemini CLI.
- **MCP server stateless.** Nothing in ferrobot needs MCP sessions or
  server-to-client notifications; stateless removes the GET stream that broke
  Gemini CLI and the session-lost case after a daemon restart.

## F4c: archive

- **Keyed by sandbox name** (`archives/<name>/`), so a wake needs no stored
  reference: `create` looks for an archive first. A new agent with the same
  name as an agent deleted with `keep_data=true` would get its archive, the
  same way it would get its kept home volume today.
- **Encrypted at rest** since the hardening pass (see item 3 below).
- **Old token retired on archive**, and blanked in the image config
  (`docker commit` copies the container env into it).

## F2: guest agent

- **socat + a Node script** instead of a guest binary: Node has no AF_VSOCK,
  socat (in Debian) does; one process per connection keeps the agent tiny.
  The agent runs as `node`, like `docker exec` runs as the image user.
- **JSON with base64 payloads, one request per connection**; answers up to
  64 MiB (files ~48 MiB), exec output capped at 1 MiB per stream in the guest.

## Agent briefing, mentions, global memory

- **`create_agent` takes an optional `task`.** After approval the daemon sends
  it as the creator's first message to the new agent, so it starts at once and
  its replies thread back to the creator. Without a task the result says the
  agent stays idle. Seen live: a creator put the task in `instructions` only,
  and both new agents slept without ever getting a turn.
- **Questions go up the chain by prompt, not by force.** The shared system
  prompt tells briefed agents to send questions and results to the teammate
  that briefed them, and creators to relay operator questions with `ask_user`.
  `ask_user` itself stays open to every agent (the operator may chat with a
  sub-agent directly).
- **Mentions:** agents were told to address teammates by id, so they wrote
  ids and bold names. The prompt now asks for `@name` in prose (ids only in
  `send_message.to`); the daemon already stored `@name` as a chip token.
  Streaming text shows plain `@name` until the turn's final text arrives.
- **Global memory is a separate table** (`global_memories`, v8, own FTS) rather
  than a nullable `memories.agent_id`, which would need a table rebuild.
  `remember`/`forget` take `global: true`; `recall` searches both scopes,
  alternating hits so neither crowds out the other, and marks global ones.
- **Agents may write global memory, but only change their own entries.**
  That is what makes it useful (one agent learns where the operator flies
  from, all know). Each entry records its author (v10; `NULL` = the
  operator); agents cannot overwrite or delete entries of other agents or of
  the operator, `recall` says who wrote each one, and the Global memory dialog
  shows it. Remaining risk: a hijacked agent can still add new entries every
  agent reads; `recall` calls them notes, not instructions.

## Connectors

- **The daemon is the MCP gateway.** It connects to each connector as an MCP
  client (`rmcp` client + reqwest transport, already in `Cargo.lock` for the
  tests) and offers its tools through ferrobot's own MCP server as
  `<connector>__<tool>`. No driver, protocol frame or runner change, and the
  token never leaves the daemon. A reverse proxy per connector would have
  needed MCP config in all four drivers.
- **A static Bearer token or an OAuth sign-in; remote Streamable HTTP only.**
  Local stdio servers are not supported (they would run in the sandbox and
  need new packages there). Put a secret in the token field, not in the URL:
  URLs are stored in SQLite and may show up in daemon logs.
- **Per-agent assignment** (`agent_connectors`, v9), least privilege: an agent
  only gets connectors the operator ticked. The assignment is checked again
  on every call, not just when listing. Agents created by an agent inherit its
  connectors, like the provider.
- **One upstream session per agent and connector,** so stateful servers never
  share state between agents. Sessions are dropped when a connector's URL or
  token changes, when it is deleted and when an agent's assignment changes.
- **Failure handling:** listing waits at most 5 s per connector (Claude lists
  tools every turn), slow or broken connectors are skipped, and the agent's
  own tools always work. Calls time out after 120 s and are never retried
  (tools have side effects). Upstream tool names that a driver cannot use
  (characters outside `A-Za-z0-9_-`, or longer than 64 with
  `mcp__ferrobot__`) are skipped with a warning.
- **Connector tools are auto-approved,** like every ferrobot tool
  (`permissions.ts` allows `mcp__ferrobot__*`, Codex runs with
  `default_tools_approval_mode = "approve"`). The operator decides per agent
  instead.
- **Codex and Gemini CLI may keep their first tool list** until the runner
  restarts, so assignment changes reach them later than Claude and the
  OpenAI-compatible driver (both re-list per turn). Gemini CLI tool calls of
  connectors show only the connector name in the chat (its call ids are
  parsed up to the first `__`).

## Connector OAuth

- **rmcp's own OAuth client** (`auth` feature): discovery (RFC 9728 protected
  resource metadata, RFC 8414), dynamic client registration, PKCE S256, the
  RFC 8707 `resource` parameter, RFC 9207 issuer checks and token refresh.
  New crates: `oauth2` 5.0 (MIT/Apache-2.0) plus older majors of `rand` and
  `thiserror` it needs. Hand-rolling the flow would have been more code and
  more ways to get it wrong.
- **The daemon is the OAuth client.** Tokens and the registered client live in
  the secret store under `connector-oauth:<id>`; the gateway sends them, so
  they never reach a sandbox. A static token wins over a sign-in.
- **Redirect to the daemon's own loopback UI listener**
  (`http://127.0.0.1:<port>/v1/oauth/callback`, RFC 8252). It needs no
  operator token (a browser redirect cannot carry one); the single-use
  `state` of a sign-in the operator started authorizes it, valid for 10
  minutes and only in memory, so a daemon restart cancels open sign-ins. The
  page is static text with `default-src 'none'`. A daemon on another machine
  cannot be signed in this way (the browser must run where the daemon does).
- **SSRF guard stays on** for public connectors: rmcp refuses metadata that
  points at private addresses. Connectors whose own URL is loopback or a
  private IPv4 address (the operator chose it) skip the guard.
- rmcp logs the authorization code at `debug` level; the daemon's default
  filter (`info,ferrobot=debug`) does not include it.

## Model proxy hardening (security review)

- **Allowlist of calls per API family:** Anthropic `POST v1/messages`
  (+ `count_tokens`), `GET v1/models`; Bedrock `model/<id>/invoke*` and
  `count-tokens`, control plane `GET inference-profiles`; Vertex only
  `:rawPredict`/`:streamRawPredict` below the provider's own project and
  region; OpenAI-style `chat/completions`, `GET models`; Codex `responses`
  (+ `compact`), `GET models`; Gemini CLI `v1beta/models/<m>:{generate,
  streamGenerate,count}*`. Anything else is 404, so a hostile sandbox cannot
  use the key for files, batches, fine-tunes or other GCP projects.
- **Paths are plain** (`A-Za-z0-9-._~:@/`, no empty or dot segments) and the
  final URL must keep the upstream's origin and base path: double-encoded
  `..` and tab/CR/LF tricks no longer leave the base.
- **No redirects** (a redirect carried `x-api-key` to another host), and
  **header allowlists** both ways (no `cookie`, `openai-organization`,
  `x-goog-user-project` out; no `set-cookie` back).
- Risk: an SDK update that calls a new endpoint gets a 404 until it is added
  here.

## Messages during a turn (steering)

- **A message from the operator that arrives while the agent works is handed
  to the running turn** (`steer` frame) when the runner says it can take it
  (`hello.steering`). The model sees it as "sent while you were working" and
  is told to acknowledge it in one line and act on it. Messages from other
  agents and routines still wait for the next turn.
- **The runner reports what the model saw** (`turn_done.steered`); everything
  else goes back to the queue without counting a delivery attempt, so old
  runners, drivers without steering and failed turns keep the old behaviour.
- **The OpenAI-compatible driver** adds late messages before its next model
  call, and after a final answer it makes one more call so the reply covers
  them. **Claude** steers too since hardening item 6; Codex and Gemini CLI
  queue as before (see item 6).
- **After the OAuth review:** sign-in and token use need published metadata
  (no rmcp endpoint fallback, which has no issuer to check), refreshes of one
  connector take turns and a revoked refresh token signs it out, sign-in
  links must be http(s), a URL change drops the tokens, a duplicate `state` is
  refused, and loopback connectors keep rmcp's private-address guard.
- ~~Not fixed: DCR client secret, revocation~~ (both done in the hardening
  pass, item 5).
- **Desktop links** (Markdown, sign-in pages) open in the system browser; the
  window itself only navigates within the app, so the injected operator
  token never reaches another page.

## Shared files and HTML pages

- **Agents hand files over with `share_file`.** The daemon copies the file
  (up to 10 MB) from the sandbox into object storage
  (`artifacts/<agent>/<uuid>`, the same store as snapshots; local FS by
  default) and records a `file` event, shown as a file card in the chat.
  The copy outlives sleep, archive and the sandbox; deleting the agent
  (without keeping data) deletes its files. Bare names resolve like the
  agent's tools see them (`<home>/workspace/<name>`); the backend reports its
  home (`SandboxBackend::home`).
- **Reads are capped** at 32 MiB in every backend (`read_file`), so a file an
  agent names cannot exhaust the daemon.
- **Markdown and text open inline; `.html` opens as a page** at
  `/artifacts/<agent>/<file>?exp=<unix>&sig=<hex>`: outside `/v1` (a frame
  or the system browser cannot send the operator token), authorized by a
  signed capability: HMAC-SHA256 over `<agent>/<file>/<exp>` with a 256-bit
  key from the keychain (`artifact-link-key`, made on first use), valid for
  30 minutes. `GET /v1/agents/{id}/files/{file}` (operator token) hands out a
  fresh link each time. A stolen link stops working after its expiry; the
  key never leaves the daemon. Served with `Content-Security-Policy: sandbox
  allow-scripts` (opaque origin, no same-origin), inline scripts, styles and
  `data:`/`blob:` media only, `connect-src 'none'`, no forms. The allowlist
  of remote origins is empty on purpose: any allowed origin is a channel out
  of the page and a script supply chain. Risk left: a page can still show
  misleading content (phishing-like UI).
- **Storage keys are checked on read.** `files::read` only reads
  `artifacts/<agent>/<uuid>` of the agent the event belongs to, so a forged
  or corrupted `file` event cannot point at another agent's files, a
  snapshot or `../`.
- File names in an agent's message (in backticks) open the shared file of
  that name in the chat; the latest share wins.
- **Every shared file has a capability URL** (`url`): pages open sandboxed,
  a file named `.pdf` that starts with `%PDF-` opens inline (no CSP sandbox,
  which would stop browsers' PDF viewers; `nosniff` keeps it a PDF), anything
  else or `&download=1` is an attachment under `sandbox; default-src 'none'`.
  The desktop opens pages and downloads in the system browser (the window
  cannot save downloads) and PDFs in a full-screen viewer.
- **`ferrobot-pdf`** in the runner image renders HTML to PDF with Debian's
  Chromium (`--no-sandbox`: the container is the sandbox). The image grows
  from about 1.3 GB to about 2 GB.

## Hardening after the external review (Nov 2026)

- **Item 1: shared-file links are signed and expire** (see "Shared files and
  HTML pages"). 30 minutes is long enough to read a page and short enough
  that a link copied out of a log or history is dead by then. No
  per-link revocation: rotating `artifact-link-key` in the keychain kills
  every link.
- **Item 2: `Settings` has a hand-written `Debug`** that prints
  `[REDACTED]` for `anthropic_api_key` and `api_token`. It destructures every
  field, so a new field does not compile until someone decides whether it is
  secret. `AppState`'s derived `Debug` goes through it.
- **Item 6: the Claude driver steers** through the Agent SDK's streaming
  input: the turn's prompt and every late message go in as user messages
  with a uuid, `priority: "next"` (folded into the running turn between tool
  rounds) and `client_composed` (the text is not read as a slash command or
  `@`-mention, whoever wrote it). The query ends once every uuid shows up in
  a result's `user_message_uuids`, so a message that lands after the final
  answer gets its own answer in the same runner turn. Any result answers the
  turn's own prompt, so only late messages can hold the query open; a CLI
  that does not report `user_message_uuids` ends after the first result.
- **Codex and Gemini CLI do not steer;** their messages wait for the next
  turn as before. ACP's `session/prompt` takes no further input once sent
  (only `session/cancel`). Whether Codex 0.166.0's app-server can add input
  to a running turn was not checked (the Docker engine was down; `codex
  app-server generate-json-schema` in the runner image would tell).
- **Connector tools refresh after an assignment change:** the daemon sends
  `configure` again, and the Codex and Gemini drivers start a fresh process
  for the next turn (the thread or session resumes by id), which lists the
  tools anew. Claude and the OpenAI-compatible driver list tools every turn.
  Changes on the connector's side (its server adds a tool) still show up only
  with a fresh process.
- **Item 3: archives and snapshot home tars are encrypted** before they
  reach `--storage-url` (`ferrobot_sandbox::seal`): AES-256-GCM (ring) over
  64 KiB chunks, STREAM-style nonces (random 7-byte prefix per write,
  counter, last-chunk flag) and the object key as AAD, so a dropped,
  reordered or appended chunk, or an archive moved to another agent's path,
  fails to open. Images stream through it (about 1.3 GB, never in memory).
  The key is `archive-key` in the secret store, made on first start and
  never replaced: with a stored value that is not a key, a remote store
  stops the daemon, and the local store warns and writes new archives
  unencrypted (old ones fail to open until the key is fixed); nothing is
  orphaned.
- **Remote storage needs the key.** With an `s3://` (or any non-local)
  `--storage-url` the daemon refuses to start without the key; with the
  local data dir it warns and writes plaintext (the disk is the daemon's
  anyway). Objects without the header (written before this change) are read
  only from a local store; a remote store refuses them, so nobody with
  bucket write access can swap in an unencrypted image.
- **The retired token:** rotated on archive (tested: the old token gets 401
  on `/mcp`) and blanked in committed images (`ENV FERROBOT_AGENT_TOKEN=`;
  the container env sets the current one). Not stripped: files in the home
  directory (e.g. tool configs) may still contain the old, dead token.
- **Not encrypted:** files agents share (`artifacts/`). They are small,
  meant to be read and served, and the review scoped this to archives; with
  S3 they would sit there in the clear. Snapshot images stay in the local
  Docker engine and are not encrypted either.
- **Losing the key loses the archives.** It lives in the OS keychain (or
  `secrets.json` with `--secret-store file`); there is no export or escrow.
- **Item 4: Codex's `auth.json` lives in the secret store**
  (`codex-auth:<provider>`), not in `<data dir>/codex`. Each `codex
  app-server` run gets a fresh `CODEX_HOME` under `<data dir>/codex/run/`
  (0700, `auth.json` 0600), removed when the run ends; leftovers from a crash
  are removed at start. What Codex wrote (a sign-in, refreshed tokens) is
  stored back, unless another run replaced the stored sign-in meanwhile, so a
  stale refresh cannot undo a new sign-in. Sign-ins in the old place move
  over on first use; deleting the provider deletes its sign-in. Not done:
  locking runs per provider; two runs refreshing at once could each use the
  refresh token (the old shared file had the same race).

- **Item 5: a client secret from dynamic registration is kept**
  (`connector-dcr:<id>`, keychain). The daemon registers the client itself
  (rmcp's `register_client`, same scopes rmcp would pick) instead of letting
  rmcp's session do it, because rmcp drops the secret. It is stored only when
  the sign-in succeeds, so an abandoned sign-in cannot orphan the tokens of
  the current client, and it is used for refresh and revocation only for
  the client id it came with. Sign-out, a URL change and delete forget it.
  Revocation itself (RFC 7009) landed with the connector OAuth apps PR.

## Connector OAuth: own apps, scopes, expiry, revocation

- **Own OAuth app per connector** (client id, optional secret, scopes) for
  providers without dynamic client registration (Google, GitHub, Slack). It
  is stored as one Keychain entry (`connector-client:<id>`), so no migration.
  rmcp's `with_preregistered_client` / `with_client_secret` / `with_scopes`
  build the request; rmcp stores no client secret, so the daemon re-applies
  it (`configure_client`) before every session, which makes refreshes of
  confidential clients work. Changing the client id signs the connector out.
- **Google:** sign-in URLs to `accounts.google.com` get
  `access_type=offline&prompt=consent`, else Google issues no refresh token.
- **Expired sign-ins** are kept as the client registration plus an `expired`
  flag when the server rejects a refresh token, so the UI can say "sign in
  again" instead of silently showing "not signed in".
- **Sign-out revokes at the server** (RFC 7009) through the `oauth2` crate
  (already a dependency of rmcp, same version): the refresh token, else the
  access token, only to an https endpoint on the token endpoint's origin
  (`oauth2` itself refuses non-https). Best effort: sign-out always forgets
  the tokens locally.
- Sign in opens the system browser directly; the connector list shows the
  redirect URI to register with an own app.
- **An own app is bound to an issuer** the operator enters (e.g.
  `https://accounts.google.com`). Sign-in, refresh and revocation are refused
  when the server's metadata names another issuer; rmcp checks that the
  metadata was served by that issuer (RFC 8414). Without this, a hostile MCP
  server could point the authorization endpoint at Google and the token
  endpoint at itself and receive the code, the PKCE verifier and the client
  secret (found by the OAuth review, verified against a fake server).
- **Revocation only to the issuer that issued the tokens** (stored issuer =
  discovered issuer), to an address resolved once, pinned and checked: no
  private, loopback, link-local or CGNAT address unless the connector itself
  lives on this machine or in the LAN.
- **A changed URL, client id or issuer drops the stored client secret**
  unless the same request sends a new one, and a sign-in that was started
  before such a change is discarded when the browser comes back.
- **Deleting a connector revokes its tokens** (like sign-out) and removes
  its secrets before the row, so a keychain error leaves it to retry.

## Agent collaboration: wake, tasks, names, greeting

- **Mail from an agent wakes a sleeping one** (paused, stopped or archived)
  unless it is sent with `priority: false`. That already held; it is now
  tested through the real `send_message` tool and said in the shared prompt
  and the tool description. No new tool.
- **Tracked tasks instead of trusting the model to report back.**
  `assign_task` (and `create_agent` with `task`) queue the task message
  together with a `task` event on the delegate, in one transaction. A
  separate tool rather than a flag on `send_message`: a name the model can
  pick for "hand this over" is clearer than a boolean.
- **No migration:** the marker is an event, and a task's state is derived
  from the mailbox: `queued`/`working`/`failed` from the message, `done` when
  the delegate sent the assigner a message threaded to the task (directly or
  to a follow-up threaded to it), `open` otherwise. The events have no index
  on `kind`, so listing tasks, and the supervisor's reminder pass on every
  tick, scan the events table and every delivered task; fine at this size,
  an index or a closed-task marker (a migration) if it grows.
- **Automatic report at the end of the turn.** When the delegate's turn on
  the task (or on a follow-up the assigner threaded to it) ends, the daemon
  sends its final answer to the assigner as a normal message from the
  delegate, `reply_to` the task, so it wakes a sleeping assigner or waits
  behind its running turn. Stopped by the operator and failed for good are
  reported too (short); a failed turn that is retried is not. No report when
  the delegate already answered in the thread, so nothing arrives twice; the
  prompt says to use `reply_to` only for the final answer, since an interim
  update with `reply_to` would replace the report. The report is queued
  before the task message is marked delivered, so the supervisor never sees
  a finished task without an answer in between.
- **No loops:** a report is a plain message, never a task, and the
  assigner's turn on it is not threaded to a task it received.
- **One reminder:** a delegate whose turn ended without an answer (empty
  result, no reply) gets one message from the supervisor, sent as from the
  assigner and threaded to the task, once it is idle (no turn, nothing
  queued that would wake it); its turn then reports like the task's. It
  shows in the assigner's chat as a message it did not write. A `task_nudged` event
  keeps it to one. Not for interrupted tasks: that would restart work the
  operator stopped.
- **Names:** the prompt and `create_agent` ask for short, memorable,
  lowercase names that fit the role (`scout`, `forge`, `ledger`, `echo`).
- **Greeting:** a new agent's first turn opens with a `[ferrobot]` line
  asking for a one- or two-line greeting to the operator (who it is, what it
  will do now), so the task turn of an agent created with a task starts with
  it. An operator-created agent with nothing queued when its runner first
  connects gets a turn of its own for it: a `turn` frame with
  `message_id: 0` and no mailbox message, so the operator sees only the
  agent's greeting in its chat (a trigger message would show as a bubble
  the operator never wrote). It costs one model call per creation and is not
  retried. `Settings::greet_new_agents` turns it off; the in-process test
  harness does, because fake runners expect the operator's message first.

## Release prep (v0.1.0)

- **Committed images lose every copy of the agent token,** not only
  `FERROBOT_AGENT_TOKEN`: the archive and snapshot paths read the container's
  env and blank each variable whose value is the token (today also
  `ANTHROPIC_API_KEY`, `GEMINI_API_KEY` and `AWS_BEARER_TOKEN_BEDROCK`). Matching
  by value instead of a name list means a new driver variable cannot slip past.
- **Dependency audit in CI** (`cargo audit` on both lockfiles, `npm audit` on
  both packages): vulnerabilities fail the job. `time` was updated to 0.3.55
  for RUSTSEC-2026-0009. Unmaintained crates stay warnings because no
  maintained release exists: `chrono` and `chrono-tz` (routines' time zones,
  `croner`), `humantime` (via `object_store`), `zbus_names`/`zvariant` (the
  keyring's Linux backend), and in the Tauri lockfile `proc-macro-error`, the
  `unic-*` crates and `glib` 0.18 (unsound iterator, Linux GTK stack of Tauri).
