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
- **Subscription logins (Claude OAuth) are not built**, per Tim's rule. Since
  2026-11-23 an opt-in Claude Code provider runs the host's own, unmodified
  `claude` with the user's own login (Tim's call); ferrobot still never reads,
  stores or relays Claude credentials. Anthropic's legal page does not
  "prevent an end user from signing in to the unmodified Claude Code binary
  with their own Claude subscription"; it forbids offering Claude.ai login or
  routing requests through plan credentials on behalf of users.

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
  rails and the daemon's tool check mostly do not apply to Codex's own
  commands; the container stays the boundary. Requests Codex does send are
  still answered with `decide()`. Because a force-push or publish would get no
  card, a Codex agent never gets a CLI account (0.1.11 security review), and
  the provider's hint says Codex runs most commands without asking. Routing
  Codex's shell through the gated `bash` MCP tool, as Claude Code does, is open.

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
- **Agents may write global memory and change each other's entries, never the
  operator's.** That is what makes it useful (one agent learns where the
  operator flies from, all know). Each entry records its author (v10; `NULL` =
  the operator); agents cannot overwrite or delete the operator's entries,
  `recall` says who wrote each one, and the Global memory dialog shows it.
  Until 0.1.11 an agent could only change its own entries; Tim asked for
  agents that keep the operator's knowledge base current on their own
  (2026-11-26), which needs refreshing and removing what another agent wrote.
- **The knowledge base is in every memory agent's instructions.** The daemon
  appends the rule to keep it current and the newest 50 global entries (300
  characters each) to the instructions of agents with the Memory group, and
  re-sends them (`Configure`) on every change, so no runner change was needed.
  Remaining risk, larger than before: a hijacked agent can write an entry that
  now reaches every memory agent's system prompt, not only a `recall` result.
  The block calls the entries notes, not instructions, that never override
  theirs; the operator sees and edits them in Settings → Global memory.

## Desktop (roadmap item 4, 0.1.11)

- **An option of the CLIs image, not a second image.** Settings → CLIs → Desktop adds Xvfb,
  Openbox, tint2, lxterminal, Thunar, the Arc-Dark GTK theme, Inter, JetBrains Mono, a small CJK
  font, xwallpaper, x11vnc, socat, xdotool, scrot and procps to the shared image (Chromium is
  already in the runner image for PDFs), about 170 MB. The programs start on demand
  (`desktop-start.sh`, idempotent), since a stopped computer loses them.
- **Its look is written at the first start, from files only.** Openbox (our own dark theme and
  title buttons), a tint2 dock and lxterminal are configured by plain files, so no D-Bus or
  xfconf runs; the wallpaper and two icons are drawn there by a short Node script, so neither
  the image nor the repo carries picture files. A first, icewm-based look was replaced in the
  same release (Tim: too plain); it cost about 50 MB less. Debian's `chromium` wrapper ignores
  flags from the environment, so `~/.local/bin/chromium` passes `--no-sandbox` and the rest.
- **VNC over a streaming exec, no port.** Each screen connection runs x11vnc in inetd mode; its
  inetd mode writes to stdin, so socat gives it a socketpair, and `DISPLAY` carries the display
  because socat reads `:` as a separator. x11vnc ignores socat's SIGTERM at the end, so the
  wrapper kills it by its own log name. `-unixsock` would avoid that but needs a patched
  libvncserver. Watching connections get `-viewonly`, enforced by the server.
- **Control belongs to a connection.** `?control=true` opens a non-view-only server and holds
  `Desktop::take` until the WebSocket closes, so a closed window never leaves the agent paused;
  the agent's `desktop` tool returns "your operator is using your desktop" instead of waiting.
- **Agent actions run in the daemon** (xdotool, scrot through exec), so the runner image is
  unchanged; a screenshot is an MCP image result. Whether a driver passes it to the model is the
  driver's business (not checked per driver yet).
- **Opt-in and never with a CLI account.** `ToolGroup::defaults()` leaves Desktop out, so NULL
  tools (every default group) never include it. Typing into the desktop's terminal skips
  `tool_check`, and with a login's token swap that could push or publish without a card, so the
  daemon refuses the pair in both directions. Tim decides at review whether to relax this.
- **noVNC 1.7.0** (`@novnc/novnc`, MPL-2.0, ESM, no dependencies; approved by Tim) draws the
  screen in the UI. The bundler drops its license headers, so its notices (MPL-2.0, and the MIT
  and BSD-style terms of the pako and DES code it bundles) are in
  `apps/desktop/public/THIRD-PARTY-NOTICES.txt`, which the app bundle and the repo carry.

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
  RUSTSEC-2026-0592 (`url` 2.5.8, stack overflow in `Url::origin()` on nested
  `blob:` URLs) is fixed with `url` 2.5.9, taken on 2026-11-24, three days
  before the 7-day publish age, as a one-off exception (Tim's call): a
  connector server's redirect reached `origin()` through `rmcp`, so it could
  crash the daemon.
  RUSTSEC-2026-0594 (`cookie` 0.18.2 via `tauri`, private-cookie nonces can
  repeat across `fork`) is ignored in `.cargo/audit.toml` until 0.18.3 passes
  the 7-day publish age (2026-11-30), Tim's call.

- **The desktop app bundles the daemon** as a Tauri sidecar
  (`tauri.release.conf.json` adds it, so dev builds need no binary). The shell
  starts it only when nothing answers on `127.0.0.1:7878`, with the OS app
  data dir as `--data-dir` (macOS: `~/Library/Application Support/dev.ferrobot.desktop`),
  `--driver claude`, `--agent-listen 0.0.0.0:7879` and the release's runner
  image, logs to `daemon.log` there, and stops it on exit (SIGTERM, then kill
  after 5 s; Windows: kill). Ports stay fixed because the CSP and the UI's
  default URL name them.
- **The daemon starts without Docker** and logs an error instead of exiting;
  `/v1/health` reports `sandbox_ready` and `sandbox_error` (live ping, 2 s
  timeout). Without a socket at start the client points at a port that never
  answers, and each ping tries the real socket again, so Docker started later
  is picked up without a restart. When the shell sees `sandbox_ready: false` at start it opens
  `docker.html` (a static page in `public/`, no UI code), which links Docker
  Desktop and returns to the app once health says ready.
- **Release builds use `ghcr.io/byquexo/ferrobot-runner:<version>`,** pushed by
  the tag workflow (amd64 and arm64 on native runners, one manifest). Docker
  pulls the configured runner image when the engine lacks it, on the first
  agent start; archive images are never pulled.
- **Release workflow on `v*` tags:** a draft release first (so both desktop
  jobs upload to it), macOS universal `.dmg` (daemon built for both arches and
  joined with `lipo`) and Windows NSIS `.exe`, then `SHA256SUMS`. Signing is
  optional: Apple and Windows certificate secrets are used only when set,
  otherwise the build is unsigned and the README explains how to open it.
- **Windows build of the sandbox crate:** the Firecracker module keeps
  compiling with a stand-in for Unix sockets that always fails; Firecracker
  needs Linux anyway.
- **Files the page builds (blob URLs, e.g. the usage CSV) save to Downloads**
  through the shell's download handler; other downloads are refused.

## Voice

- **The UI speaks and listens, the daemon calls the provider.** With voice on
  in a chat, every `assistant_text` that arrives live (interim updates and
  the final reply, never history) is spoken in order; the mic button records
  a message, the daemon transcribes it and the UI sends the text like a typed
  one. Pressing the mic or switching voice off or changing chats stops the
  audio. Spoken messages reach a running turn only where typed ones do
  (steering: the OpenAI-compatible and Claude drivers); with Codex and
  Gemini CLI they wait for the turn to end.
- **Providers: ElevenLabs and OpenRouter** (both checked against their docs
  on 2026-11-20):
  - ElevenLabs speech: `POST /v1/text-to-speech/{voice}/stream?output_format=mp3_44100_128`,
    `xi-api-key`, `{text, model_id}`, default model `eleven_flash_v2_5`
    (https://elevenlabs.io/docs/api-reference/text-to-speech/stream,
    https://elevenlabs.io/docs/overview/models). No default voice: the docs'
    example voice expires on 2026-12-31 and voices differ per account, so the
    operator enters a voice id.
  - ElevenLabs transcription: `POST /v1/speech-to-text`, multipart `model_id`
    + `file`, answer `{text}`, default `scribe_v3`
    (https://elevenlabs.io/docs/api-reference/speech-to-text/convert).
  - OpenRouter speech: `POST /api/v1/audio/speech`, Bearer,
    `{model, input, voice, response_format: "mp3"}` (its default is pcm,
    which `<audio>` cannot play); default `mistralai/voxtral-mini-tts-2603`
    with `en_paul_neutral`
    (https://openrouter.ai/docs/guides/overview/multimodal/tts).
  - OpenRouter transcription: `POST /api/v1/audio/transcriptions`, multipart
    `model` + `file`, answer `{text}`, default `openai/whisper-large-v3`
    (https://openrouter.ai/docs/guides/overview/multimodal/stt).
- **No OpenAI provider.** Its `/v1/audio/speech` models all shut down on
  2027-01-06 (https://developers.openai.com/api/docs/deprecations) and the
  same models are reachable through OpenRouter.
- **Keys** live in the secret store as `voice:elevenlabs` and
  `voice:openrouter`, write-only over `PUT /v1/voice` (`has_key` per
  provider), separate from model provider keys. **Settings** (provider,
  models, voice) are JSON under the key `voice` in a new `settings` table
  (v12), so later voice fields need no migration.
- **Multipart is built by hand** (about 15 lines): reqwest's `multipart`
  feature would add `mime_guess`. No new crates.
- **Limits so a stuck UI stays cheap:** speech text at most 4096 characters
  (the UI cuts replies at about 1500, at a sentence), recordings at most
  10 MiB and 2 minutes, and per minute at most 6000 characters of speech
  (about four times listening speed) and 20 transcriptions; above that the
  API answers 429. Provider calls time out after 120 s. Neither text nor
  audio is logged; a provider's error reason goes to the operator only.
- **Playback waits for each clip** (fetch, then `<audio>` with a blob URL)
  instead of streaming through `MediaSource`; the next two clips are fetched
  while one plays.
- **Tauri:** the CSP allows `media-src 'self' blob:`, and
  `src-tauri/Info.plist` carries `NSMicrophoneUsageDescription` (Tauri
  embeds it in dev builds and merges it into bundles). wry 0.55 grants the
  page's microphone request itself. Not tried in the real window; a signed
  build with the hardened runtime also needs the
  `com.apple.security.device.audio-input` entitlement.
- **Two buttons: call and dictation (Tim's feedback after the first test).**
  The phone button in the chat header starts a hands-free call: replies are
  read aloud and, with ElevenLabs, the microphone streams to its realtime
  transcription (`scribe_v3_live`, commit on a 1.2 s pause); each finished
  sentence is sent. The microphone stays live while an agent speaks, so the
  operator can cut in (see "Voice call feel"). The mic
  button in the message box is dictation: the words appear live in the box,
  stop keeps them there unsent. Other providers transcribe after stopping.
- **Live transcription with a single-use token.** `POST /v1/voice/live`
  asks ElevenLabs for a `realtime_scribe` token (consumed on use, valid 15
  minutes, session capped at 10 minutes) and returns the WebSocket URL with
  it; the app connects directly, the real key stays in the daemon and the
  daemon relays no audio. The CSP allows `wss://api.elevenlabs.io` only.
  Audio is 16-bit mono PCM at the AudioContext's rate (16 kHz asked for)
  through a ScriptProcessor, because an AudioWorklet needs a module file
  the CSP would block. Not verified against the real service yet: the
  token endpoint's auth (the docs show none; the daemon sends `xi-api-key`).
- **Catalog and quick changes.** `GET /v1/voice/catalog` lists the account's
  voices (`/v2/voices`, up to 3 pages of 100, sorted by name, with verified
  languages) and its text-to-speech models (`/v1/models`). Settings and a
  small picker in the chat header choose voice, language and speed from it;
  a "Custom…" entry still takes any id. Speed (`voice_settings.speed`) is
  limited to 0.7-1.2, ElevenLabs' own UI range (the API docs state no
  range); language (`language_code`) is skipped for `eleven_multilingual_v2`,
  which refuses it. OpenRouter gets neither.
- **Voice can be switched off** (`enabled`, default on, also for settings
  saved before): speech, transcription, live sessions, the catalog and new
  calls answer 409, the app hides the call and mic buttons; keys and
  settings stay.
- **Voice on is a call; agents bring teammates in.** Switching voice on in a
  chat starts the operator's voice call with that agent (`PUT /v1/call`,
  in memory, so a daemon restart ends it); hanging up ends it, switching
  chats does not. Agents in the call have an `invite_to_call(agent, topic)`
  tool: the teammate joins and gets the topic as mail from the inviter,
  which starts its first turn. At most 4 agents per call. What the operator
  says or types goes to all of them (`POST /v1/call/messages`), and every
  agent's replies are spoken, each with
  its own voice if one is set (`agent_voices`, by agent id).
- **Agents in a call never start each other's turns.** A deferred message
  (`priority: false`) is no guard: an awake runner claims it at once, so two
  agents forwarding replies to each other would loop and burn money. Instead
  the daemon keeps what each agent said (its `assistant_text`, cut to 1500
  characters, at most 12 KiB per listener, oldest dropped) and puts it in
  front of the operator's next message to each other agent ("In the call
  since your last turn: ..."). Only the operator's messages and invitations
  start turns. The invitation tells the teammate to answer in its reply
  (spoken), not with `send_message`.
- **Costs and open questions:** each message in a call with N agents starts
  N turns, and agents that were not addressed will probably still answer.
  Options: route by spoken name, or let agents stay silent with an empty
  reply. Agents also hear each other only with the operator's next message,
  so the call is operator-driven, not a free discussion.
- **Follow-up:** a remove button per call participant
  (today: end the call); telling the host agent that the operator is
  listening (it does not know until someone joins).

## Agent list: order and delete

- **The order lives in the app** (`localStorage`, `ferrobot.agent-order`),
  not in the daemon: there is one desktop per daemon today, and a column
  would need a migration while v12/v13 are taken by open branches. Pinned
  agents stay first; dragging only reorders within the pinned strip or the
  list. New agents go after the ordered ones, by name.
- **Right-click on an agent** opens Move up / Move down (the keyboard way to
  reorder) and Delete…, which asks for confirmation first.

## Model picker

- **Provider lists:** `GET /v1/providers/{id}/models` asks the provider's own
  list endpoint below the same upstream the proxy uses, with the stored key:
  Anthropic `/v1/models?limit=1000` (`x-api-key`, `anthropic-version`; the
  default page is 20), the OpenAI family `{base}/models` with a bearer key
  (Gemini through its OpenAI-compatible base; a leading `models/` is
  stripped). They all answer `{"data": [{"id", ...}]}`, so one parser.
- **Suggestions:** Bedrock, Vertex, Gemini CLI and a Codex provider on a
  ChatGPT sign-in have no list we can reach with what the provider stores.
  They, and every failed fetch (5 s timeout, 8 MB cap, non-2xx), get a short
  built-in list (`source: "suggested"`) in `crates/daemon/src/models.rs`,
  checked against the providers' docs on 2026-11-20 (URLs in the comment).
  `openai_compatible` has none, so the UI shows a text field.
- **Cache:** only fetched lists, 5 minutes, per provider, in memory. Editing
  or deleting the provider drops its entry, so a fixed key shows at once.
- **Key safety:** the upstream's error body is dropped (an OpenAI 401 echoes
  part of the key) and only the status is logged; the shared HTTP client
  follows no redirects.
- **Kind route:** `GET /v1/provider-kinds/{kind}/models` returns the built-in
  list without network or key, for the two fields that have no provider: the
  add-provider form and agents on the daemon's own `ANTHROPIC_API_KEY`.
- **Bedrock** suggestions are `global.` inference-profile ids because the
  proxy talks to `bedrock-runtime`; the docs show that form only for Fable
  5.2. **Vertex** suggestions need the `global` (or `us`/`eu`) region:
  regional endpoints serve Claude Sonnet 4.6 and older only.
- **Known limits:** OpenRouter's first page only (`shortcut:` comment);
  OpenAI's list also has embedding and TTS models, not filtered.
- **Desktop:** `Choice` in `ui.tsx` is a select with "Custom…" that turns
  into a text field, after the voice branch's `Choice`. Its own class
  (`.choice` is the connector chips), and a current id the list lacks stays
  as an extra option, so editing an agent never drops its model while the
  list loads.

## UI polish (unslop)

A polish pass with the unslop skill by Damir Sokolovsky
([@heydamir](https://x.com/heydamir)), CC BY 4.0. The audit found light
slop, so polish mode: palette, accent, logo, layout and features stay.

- **Direction:** a self-hosted desktop app where one operator runs a small
  team of AI agents, each with its own sandboxed computer. Calm, exact, a
  little warm; an operator's desk on the night shift (instruments, a
  terminal, a workbench). The moments are the animated avatars and the live
  chat (step line, streaming); Settings stays quiet.
- **Signature:** the presence dot. Settings uses the same `.dot` and `p-*`
  classes for providers and connectors: green for a stored key, a sign-in or
  no key needed, amber for an expired sign-in, grey for no key or not signed
  in. The row's text says the same, so colour is not the only cue. A Codex
  provider signed in with ChatGPT but without a key stays grey (only the
  sign-in line knows that state).
- **Type:** 12 (meta, labels, counts, hints), 13 (secondary text, form
  labels, tool cards), 14 (body), 16 (brand, chat and dialog titles), 20
  (settings page and profile titles). Weights 400 and 500; 600 only for bold
  and headings inside messages. Sentence case and no tracking: the uppercase
  tool and section labels went, and so did negative tracking on headings.
- **Grid:** spacing, gaps and fixed heights are multiples of 4 (1 to 2 px
  nudges allowed). Buttons, inputs and selects are 36 tall, small buttons
  28, icon buttons 32, settings nav rows 36. The presence dot keeps a 10 px
  minimum so its 2 px ring leaves a visible centre.
- **Icons:** lucide at 16 with a 1.5 stroke (one `.lucide` rule), 18 for
  header icon buttons and the composer; 12 only inside a 12 px text run or a
  16 px check box; empty-state glyphs 28.
- **Settings:** a 640 px column, left-aligned; providers and connectors as
  rows in one hairline box; sections split by hairlines; add forms open from
  an "Add provider" / "Add connector" button and close after adding. The
  switch uses the accent instead of the semantic green.
- **Motion kept:** the working dot's `pulse`, the step line's `breathe`, the
  streaming caret and the recording mic's pulse show live state, so they
  stay. The unused `.typing` bounce went.
- **Voice settings** use the shared `Choice`; `withSaved` keeps a saved
  voice or model id the list lacks selectable, as `modelOptions` does for
  models.

## Usage and cost

- **Counted where the money goes: in the proxy.** Every driver (Claude,
  OpenAI-compatible, Codex with a key or a ChatGPT sign-in, Gemini CLI)
  reaches its model through `/model`, so the runner reports nothing. The
  proxy wraps the upstream body: bytes pass on at once, SSE is read line by
  line (only lines with `"usage`), a JSON body is kept up to 2 MiB and read
  at the end. The row is written when the body is dropped, at its end or
  when the agent hangs up, in a spawned task.
- **One normalization:** `input_tokens` excludes cache reads and writes
  (Anthropic's meaning); OpenAI-style and Gemini counts have the cached part
  subtracted. `NULL` means the API reported nothing; nothing is estimated.
  A call is "not tracked" when it succeeded without input and output counts.
- **Not tracked:** Bedrock's streaming event stream (binary, not decoded);
  an SSE line over 2 MiB drops that call's counts; OpenAI-compatible servers
  that ignore `include_usage`, or reject it with a 400 (the OpenAI driver
  then retries without it, so each call also leaves a 400 `error` row).
  Model lists and token counts are not logged.
  Unknown or missing agent tokens leave no row.
- **Status:** `ok` (2xx and the body reached its end), `error` (upstream
  non-2xx, unreachable, a stream that broke, carried an SSE error event or
  was dropped by the agent before its end; counts seen so far are kept),
  `refused` (the proxy said no to a known agent: not allowed, too large, bad
  path, no key; stored with model `unknown`).
- **Model ids are agent text:** control characters are dropped and ids are
  cut at 200 bytes. Bedrock, Vertex and Gemini take the model from the path
  (what the upstream runs), never from the body.
- **Prices:** `config/pricing.toml`, compiled in, USD per 1M tokens per
  provider kind and model id, each with its source page and check date.
  Only what was read on the official pages; anything else is "unknown",
  never guessed. Ollama is free. Codex and Gemini CLI use the OpenAI and
  Gemini entries; a dated Anthropic id (also on Bedrock and Vertex) falls back
  to its undated entry, an OpenAI snapshot does not. The operator's
  prices (`PUT /v1/usage/prices`, setting `pricing`) replace the file's.
  Costs are computed when asked, so a price change also reprices the past.
- **Known gaps in the estimate:** one rate per token kind: Anthropic cache
  writes at the 5-minute rate, no long-prompt tiers (the file notes them),
  no batch, fast-mode or data-residency multipliers, no Gemini cache storage.
  A ChatGPT sign-in is not billed per token, so its cost is overstated.
- **Days are UTC dates.** The view's ranges start at local midnight, so the
  first day bar can hold only a few hours.
- **CSV export is a Blob download.** It works in a browser; in the Tauri
  window the shell's `on_download` handler saves blob URLs to the Downloads
  folder and refuses other downloads (untried in the window so far).
- **Known limits:**
  - The Usage view's aggregate queries share the store's single SQLite
    connection with the proxy, so a large range on a busy daemon delays model calls.
  - The price form says "no price yet" when only a fallback price applies.
  - Whether a cost is "unknown" can differ between group_by views when an
    override leaves out a cache price.

## Provider prompt caching

- **The proxy keeps caching intact:** it forwards request bodies unchanged
  (it only reads the model name) and passes the caching headers through
  (`anthropic-*`, `openai-beta`, Codex' `session_id`/`originator`).
- **Who caches already:** the Claude driver (the Agent SDK sets cache
  breakpoints on system prompt, tools and history; ours stays stable per
  agent), Codex and Gemini CLI on their own, and the OpenAI-compatible
  driver through automatic prefix caching (OpenAI from 1024 tokens,
  DeepSeek, Gemini 2.5+): it resends system prompt, tools (rmcp lists them
  sorted by name) and history in the same order. The Usage tab records cache
  read and write tokens, so hit rates are measurable.
- **OpenRouter gets two additions in the proxy** (checked against
  openrouter.ai/docs/features/prompt-caching, 2026-11-20): Claude models
  (`anthropic/…`) on chat completions get a top-level
  `cache_control: {"type": "ephemeral"}`, which turns on Anthropic's
  automatic caching there (it is off without it), unless the request set its
  own; and every OpenRouter call carries `x-session-id` = the agent's id, so
  sticky routing keeps an agent on the provider that holds its cache. Other
  providers' requests are untouched.
- **No `prompt_cache_key` for OpenAI:** OpenAI routes caches itself from
  GPT-5.6 on and says the key is not needed for caching there; Chat
  Completions support for it is not confirmed by its docs, so sending it
  risks errors for no gain.
- **Not changed:** Anthropic's 5-minute cache lifetime. Agents with turns
  further apart pay the cache write without a hit; the 1-hour option costs
  more per write (2x instead of 1.25x). Decide after measuring real turns.

## Auto mode

- **What it skips:** with `auto_mode` on (`PUT /v1/settings`, the `general`
  setting, default off) `create_agent` creates the agent without an approval
  card, through the same `agents::create` path: name check, the task length
  check, inherited connectors, provider, briefing and greeting stay as they
  are. It is an operator opt-in from the desktop's Settings, Agents.
- **Audit trail without a card:** an `agent_created` event
  (`{name, id, by, auto: true}`, `by` the requester's name at that time) goes
  to the chat the card would have gone to (`chat_of(requester)`); the desktop
  shows it as a quiet notice ("ada created scout (auto mode)"). No new table.
- **Still gated in Auto mode:** the agent limit (`FERROBOT_MAX_AGENTS`; over
  it `create_agent` still fails), the runner's command permission policy,
  secrets never entering a sandbox, connector OAuth sign-in (operator only)
  and `ask_user` cards (questions, not approvals, still go to the operator).
- **Fails closed:** an unreadable `general` setting counts as off, so the
  card shows.
- **Per approval kind.** Each kind decides its own Auto mode behaviour:
  `create_agent` and `create_routine` skip their card, `add_connector` (see
  Vault) always shows it.
- **Agent-facing text** stays true in both modes (the tool list is static per
  connection): the tool description and the Claude prompt say the operator
  approves unless Auto mode is on.

## Vault and connector cards

- **Vault is a view, not a new store.** Every key already lived in the OS
  keychain under its owner (`provider:<id>`, the connector's token and OAuth
  app, `voice:<provider>`). Settings, Vault lists them from what SQLite knows
  (providers, connectors, voice providers) with presence only, and sets or
  removes them through the existing routes plus `PUT /v1/voice/keys/{provider}`
  (a voice key without switching the chosen provider). No migration, no
  keychain moves, no new prompts. The key fields left Model providers,
  Connectors and Voice. Named, reusable credentials were considered and
  dropped: a migration, one keychain prompt per entry, and a reference an
  agent could later point at another URL.
- **`add_connector` binds the secret to its destination.** The card shows the
  connector's name, full URL and host ("the token goes only to …") next to
  the hidden field; approving creates exactly that connector with exactly
  that token in one step. Agents never refer to stored secrets, so a
  prompt-injected agent cannot ask for a token and send it elsewhere later.
- **The secret skips the answer path.** It is a question of kind `secret`
  (A2UI `TextField`, `obscured`), which only daemon cards carry: `normalize`
  (used by `ask_user`) refuses it and the MCP schema hides it. The answer
  route takes it out (`take_secret`) before `question_answered` is recorded,
  broadcast or returned; the agent gets a plain outcome message.
- **No waiting tool call.** Unlike `create_agent`, `add_connector` returns at
  once and the outcome arrives as a message: the operator may need minutes to
  fetch a token, and the card holds everything needed, so it still works
  after a daemon restart. Auto mode never skips it (it takes a key).
- **Given to the requester.** Approving assigns the new connector to the agent
  that asked (the same path as `PUT /v1/agents/{id}/connectors`). OAuth
  connectors are created signed out; the operator signs in under Connectors.

## OpenRouter's reported cost

- **The charge beats the estimate.** OpenRouter puts what it billed into every
  response's `usage.cost` (USD). The meter reads it on `openrouter` providers
  only (another OpenAI-compatible server's number is not trusted) and keeps it
  per call (`usage.cost_usd`, v14); reports add those as they are and estimate
  only the other rows. It covers models missing from `pricing.toml` and the
  long-context tiers `Price` cannot express.
- **Sanity bounds:** a cost that is not a number in 0..=1000 USD is ignored and
  the call falls back to the estimate.
- **No price fetching.** Research (2026-11-20): only OpenRouter and the AWS bulk
  price files publish machine-readable prices; Anthropic, OpenAI, Gemini and
  ElevenLabs only have pricing pages. The table stays hand-checked.

## MCP catalog

- **A static list in the UI** (`apps/desktop/src/lib/catalog.ts`), no API: an
  entry is a name, URL and sign-in kind, and "Add" uses the existing
  `POST /v1/connectors` and OAuth routes. Only vendor-run servers with
  Streamable HTTP; generic lucide icons, no vendor logos.
- **Zero-setup entries first:** OAuth servers that advertise dynamic client
  registration (Linear, Notion, Atlassian, Sentry, Supabase, Neon, Cloudflare,
  Stripe, GitLab, Hugging Face, Zapier) and keyless ones (Context7, DeepWiki,
  Microsoft Learn, Cloudflare docs, Exa). GitHub has no dynamic registration,
  so its entry takes a token in Vault. Left out: Slack (HTTPS-only redirects),
  Vercel and Figma (approved clients only), Google Workspace (developer
  preview), HubSpot and Asana (own app; loopback redirects unclear).
- **Agents and CLIs:** logged-in services reach agents through connectors, where
  the daemon holds the token; CLIs in the sandbox will be keyless (Tim,
  2026-11-20). A credential-swapping proxy for OAuth CLIs is a later option,
  only for CLIs without an MCP server.

## Self-update

- **Two paths, one button** (Settings, Updates). Release builds use Tauri's
  updater: signed archives on GitHub Releases, found through `latest.json`.
  Tim runs debug builds from his checkout, which no release can replace, so
  there the daemon pulls `main`, rebuilds and restarts itself. The UI picks the
  path from `GET /v1/update` (`source_build`) and a flag the shell injects.
- **Source updates are debug-only** (`cfg(debug_assertions)`): a release
  binary holds no code that runs git, npm or cargo or re-execs; its
  `POST /v1/update` answers 404. Routes and spec are the same in both profiles.
- **Which checkout:** the daemon's own path, `<repo>/target/debug/ferrobotd`,
  with the daemon manifest and `.git` there. Refused (409): not on `main`, or
  changed tracked files. Untracked files are fine; `--ff-only` keeps them.
- **Steps**, explicit arguments, no shell: `git pull --ff-only` (the existing
  `origin`; SSH in batch mode so a locked key fails within seconds, 2 min
  limit), `npm ci` only if `apps/desktop/package-lock.json` changed or
  `node_modules` is missing, `npm run build`, `cargo build -p ferrobot-daemon`,
  `cargo build` in `src-tauri`. `ANTHROPIC_API_KEY` and `FERROBOT_API_TOKEN`
  are removed from the steps' environment (npm runs install scripts).
- **Up to date** means `HEAD` after the pull is the commit the daemon started
  from, so a retry after a failed build still rebuilds.
- **Restart:** Unix `exec` of the same path with the same arguments,
  environment and directory, so data dir, ports and PID stay. Windows only
  says to restart by hand. The UI polls `GET /v1/update`; status lives in
  memory, so a fresh `idle` means the new daemon answers, and the app then
  relaunches itself through the process plugin.
- **The shell's daemon is a Tauri resource,** dropped on exit, on relaunch and
  when the Windows updater ends the app with `process::exit`. That path skips
  `RunEvent::Exit`; an exit handler would leave `ferrobotd.exe` running and
  locked for the installer.
- **The updater plugin loads only with a config** (`plugins.updater`, in
  `tauri.release.conf.json`): it fails without one, so dev builds skip it and
  the UI says updates aren't set up.
- **Signing key:** Tim's pair; the private key stays in
  `~/.tauri/ferrobot-updater.key`, the public key is in the release config. CI
  reads `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
  from repo secrets and builds without updater artifacts when they are missing
  (the CLI fails otherwise). `requireSignedVersion` is on: the CLI signs the
  version into the signature, so a tampered `latest.json` cannot pair a new
  version with an older signed archive; no release predates it.
  ```
  cd apps/desktop && npm run tauri signer generate -- -w ~/.tauri/ferrobot-updater.key
  # public key: ~/.tauri/ferrobot-updater.key.pub → plugins.updater.pubkey
  gh secret set TAURI_SIGNING_PRIVATE_KEY --repo byQuexo/ferrobot < ~/.tauri/ferrobot-updater.key
  gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --repo byQuexo/ferrobot
  ```
- **macOS builds `app,dmg` in CI:** the updater archive (`.app.tar.gz`) comes
  only from the `app` bundle.
- **Versions:** the Tauri CLI refuses crate and npm packages on different
  minors, so the shell moved to tauri 2.12, updater 2.14 and process 2.4. Those
  crates need Rust 1.90: the shell's `rust-version` is 1.90, the core stays 1.88.
- **Private repo:** `releases/latest/download/latest.json` answers 404 until
  the repo is public and a release is published (drafts do not count); the
  check then shows "Couldn't check: …".

## Connector checks

- **The status is a real connection.** The row's state comes from a check: the
  daemon connects with the connector's current auth (token, else OAuth tokens,
  else none) through the same client code agents use, lists the tools and
  keeps `ok` (tools agents can use), `needs_sign_in` or `failed`. Bounded by
  15 s; kept in memory only (`check: null` after a restart or a change).
- **Auth answers:** a 401/403 (with or without a `WWW-Authenticate`
  challenge) means `needs_sign_in` when no credential was sent, also for
  servers that take a token: the operator signs in or adds one in Vault. With
  a stored token it is `failed` ("the server rejected the token"): the sign-in
  button refuses token connectors.
- **Errors in our words.** `error` is one of a few fixed texts plus at most an
  HTTP status number; never the server's text or a URL, which can echo a
  token or hold a key.
- **When it runs:** inline after `POST /v1/connectors`, every PATCH (also the
  Vault token and client secret edits) and sign-out, so the response holds the
  result; in the background after an OAuth sign-in (the browser page does not
  wait) and an approved add-connector card; on demand with
  `POST /v1/connectors/{id}/check`. A check never starts a sign-in: an
  OAuth connector that is not signed in connects without auth, so no metadata
  discovery or dynamic registration happens.
- **UI:** the Connectors screen checks rows without a result when it opens and
  after a browser sign-in; no retry button (not asked for).

## CLIs in the sandbox image

- **One shared image** (Tim, 2026-11-20): Settings, CLIs picks keyless Debian
  packages (a short catalog in the UI plus free-form names); the daemon stores
  them in the `clis` setting (no migration) and the Docker backend builds
  `FROM <configured image's id>` with them. CLIs that need a login (gh, aws,
  gcloud, stripe) are out: keys never enter a sandbox, logged-in services go
  through connectors. Per-agent choice waits for tool scopes (the shell group).
- **Trust boundary:** names must follow Debian policy
  (`^[a-z0-9][a-z0-9+.-]{1,62}$`, at most 32, `package_set` in
  `crates/sandbox`), checked by the API and again by the backend. The
  Dockerfile is rendered from validated tokens only: `RUN` in exec form,
  `sh -c '... apt-get install ... "$@"' sh <packages>`, so a name is an argv
  entry, never shell text. The build runs as root; the image ends with the base
  image's own `USER` (node), so agents stay unprivileged.
- **Tag** `ferrobot-runner:clis-<sha256(base id, packages)[..16]>`: the same
  list on the same base reuses the image; a rebuilt base image gets a new tag.
  On startup the daemon rebuilds (a cache hit unless the base changed), so a
  rebuilt `ferrobot-runner:dev` reaches agents with CLIs. A failed build keeps
  the previous image in use; one build at a time (`409` otherwise).
- **Cleanup:** after each build every other `ferrobot-runner:clis-*` image is
  removed without force; one a container still uses stays until a later build
  or restart. Snapshot, archive and base images are never touched. An empty
  list means the plain base image.
- **Existing agents switch on their next wake.** The lifecycle never recreated
  a container when its image changed (`create` adopts by name, `wake` starts
  what exists). Now `wake` asks the backend `is_stale`: a container created
  from the base or a CLI image other than the current one is destroyed without
  its home volume and provisioned again (fresh token, same volume). Running
  agents are not interrupted; they switch after their next sleep (warm sleep
  after 5 minutes idle by default) or an explicit stop and wake. Sandboxes
  restored from a snapshot or archive keep their root filesystem and are never
  stale.
- **Docker only:** other backends report `supported: false` and refuse a
  non-empty list with `501`.

## Routines made by agents

- **`create_routine`** lets an agent schedule its own prompt (cron or alias, the
  daemon's time zone), so "remind me every morning" works without the
  operator opening the Routines tab. Only for itself: a routine for another
  agent would let one agent drive another's model spend.
- **Approval like `create_agent`:** a card in the chat the operator uses for the
  agent (name, schedule in words, the message) unless Auto mode is on; each
  routine is recurring model spend. The tool does not wait (like
  `add_connector`): the outcome arrives as a message, and the card holds
  everything needed, so it still works after a daemon restart.
- **Not added:** list, edit or delete tools for routines; the operator manages
  them in the Routines tab.

## Voice call feel

- **Speech starts per sentence.** The UI speaks agents' streamed `delta` text
  as soon as a sentence ends (`.`, `!`, `?` or `…` and whitespace, or a line
  break; not after abbreviations like "e.g." or "Dr.", initials, a one- or
  two-digit number such as "3." or inside a code block). In a streamed turn
  the final `assistant_text` only ends the block: its words were already
  heard, and they can differ from the stream (mentions are normalized), so
  the UI never matches them against what it said. A turn that did not stream
  is spoken from its `assistant_text`. The 1500-character cut now counts per
  turn.
- **One agent at a time.** Every operator line starts a turn per agent, so
  replies stream in parallel. The first text block to finish a sentence
  speaks; the other blocks wait and follow in order when it ends
  (`assistant_text` or `turn_done`), so voices never interleave mid-reply.
- **Barge-in instead of a muted microphone.** This replaces "silence while an
  agent speaks". The microphone stays live, with the browser's echo
  cancellation, noise suppression and auto gain. While something plays or
  loads, a partial transcript interrupts only with two or more words. It
  then stops playback at once, drops what the agents had not said yet (their
  running turns stay quiet) and calls `POST /v1/agents/{id}/interrupt` for
  the agent being heard. Other agents with dropped lines keep working
  silently: the spec named only the speaker, and their turns may be doing
  real work.
- **Echo fallback: a word check, no setting.** Echo cancellation against
  `<audio>` is unverified in WKWebView (macOS) and WebView2. Two or more
  heard words, of which 60 % or more were spoken by an agent in the last two
  seconds, count as echo. Such a partial does not interrupt and such a line
  is not sent. One-word answers ("yes") always pass when nobody speaks. The
  operator's mute button is the manual fallback.
- **Speech is fetched two clips ahead**, not all at once, so a long reply in
  many sentences does not burst the speech provider's rate limit.
- **Every spoken or typed line in a call goes through `POST /v1/call/messages`**,
  even with one agent, so the daemon adds the call instructions.
- **The call lives in the app, not the chat** (`lib/call-session.ts`).
  Switching chats or opening Settings keeps it. The call card sits in the
  sidebar under the header, in the flow: a floating window there would cover
  Usage, Settings and New agent. It shows avatars with a ring on the agent
  speaking, the duration, mute, hang up and a way back to the host's chat. In
  another agent's chat the phone button is disabled during a call. Starting
  there would replace the call, and there is no "add to call" control.
- **Agents hanging up.** A `call` event with fewer agents removes them. An
  agent that left still finishes its current turn (the goodbye may stream
  after the event). If the host leaves, the next agent becomes host. An empty
  list stops listening at once, since there is no call left to send to. The
  card stays until the running turns and the queued audio are done (20 s at
  most). The operator's hang-up stops everything at once.
- **`(pass)`** (an agent with nothing to add) is never spoken. In the chat
  it shows greyed.
- **Not handled:** dictation during a call opens a second live session that
  hears the same words.

## Voice calls that feel like a phone call

- **Same agent, tuned for talk** (Tim, 2026-11-21), not a speech-to-speech
  model: the agent keeps its tools, memory and sandbox in the call. Every call
  message starts with a call note (spoken, one to three short sentences, the
  solution first, no lists or links; longer work after the call in the chat;
  `(pass)` when there is nothing to add; `hang_up` when done). The pause that
  ends the operator's turn is 1.5 s (0.8 s cut people off mid-thought; Tim,
  2026-11-21).
- **Agents answer each other:** when an agent's turn in the call completes, the
  other agents get what they have not heard yet as a turn of their own (from
  that agent). At most four such turns in a row; then the lines wait for the
  operator's next words, which reset the count. Interrupted or failed turns
  and `(pass)` replies are not passed on.
- **Hanging up:** the `hang_up` tool takes effect when the agent's turn ends,
  so its goodbye is still spoken. The host (the agent the operator called)
  ends the call for everyone; an invited agent leaves.
- **Single-agent calls** go through `POST /v1/call/messages` too, so the host
  gets the call note; the operator's call messages lose its length to it (limit
  now 19,316, see "Call flow").

## Tool profiles

- **Groups, not single tools.** Nine: six for ferrobot's MCP tools, `team`
  (`list_agents`, `send_message`, `read_mailbox`, `assign_task`, `list_tasks`),
  `memory` (`remember`, `recall`, `forget`), `operator` (`ask_user`,
  `share_file`, `invite_to_call`, `hang_up`: everything that reaches the operator),
  `create_agents`, `add_connectors`, `routines`; and three built into the
  model's harness, `shell` (and with it every CLI in the image: one shared image
  cannot hide single CLIs), `files`, `web`. The ids `create_agents` and
  `add_connectors` keep them apart from the per-connector switches.
- **A connector's switch is its assignment** (`/v1/agents/{id}/connectors`); the
  profile has no second one. `create_agent` takes `connectors` (names, a subset
  of the creator's) next to `tools`.
- **Stored as the enabled set** in `agents.tools` (v15), NULL = every group: a
  group added later reaches untouched agents but not narrowed ones. Saving
  every group stores NULL again.
- **ferrobot tools are enforced in the daemon:** `tools/list` filters and
  `tools/call` refuses a tool outside the profile, since a model can call a name
  it was never shown. A tool without a group is never offered (a test checks
  every tool has one). Connector tools already check the assignment on both.
- **Built-in tools are enforced by the runner**, per driver. Each was checked by
  capturing the tool list the CLI sent to a fake upstream, with the real driver
  code (Codex and Gemini inside the runner image):
  - Claude (Agent SDK 0.3.328): `disallowedTools`. shell: `Bash`, `Monitor`,
    `PowerShell`; files: `Read`, `Write`, `Edit`, `MultiEdit`, `NotebookEdit`,
    `Glob`, `Grep`, `LSP`, `EnterWorktree`, `ExitWorktree`; web: `WebFetch`,
    `WebSearch`. `canUseTool` refuses them too.
  - OpenAI-compatible loop: its own `bash`, `read_file`, `write_file` are not
    offered and are refused when called anyway. It has no web tool.
  - Codex 0.166: `config.toml` `[features] shell_tool = false, unified_exec =
    false` (`exec_command`, `write_stdin`), `view_image = false` for files
    (Codex edits through its shell; no model we tried got `apply_patch`),
    `web_search = "disabled"`. Approval requests are refused too but not relied
    on: `on-request` with full access runs commands without asking.
  - Gemini CLI 0.67 over ACP: `tools.exclude` in `~/.gemini/settings.json`
    (`run_shell_command`, `list_background_processes`, `read_background_output`,
    `read_file`, `read_many_files`, `write_file`, `replace`, `list_directory`,
    `glob`, `grep_search`, `web_fetch`, `google_web_search`); permission
    requests are refused too.
  - Sub-agents (Claude `Agent`/`Workflow`, Codex `multi_agent`, Gemini
    `invoke_agent`) are off while any built-in group is off: they run tools of
    their own.
  - Codex and Gemini read their config at start, so the driver restarts them
    when the profile changes. `configure` now calls `reconfigure()`; before,
    only `welcome` did, so connector changes never reached a running process.
  - No driver needed the fallback of refusing a combination. With shell on,
    files or web off only removes the dedicated tools; commands still read
    files and reach the web, and the dialog says so.
- **Old runners:** `PROTOCOL_VERSION` 2 carries `tools` in `welcome` and
  `configure`. A sandbox keeps its runner until it is recreated (a rebuilt
  `ferrobot-runner:dev` keeps its name, so `is_stale` does not notice), so a v1
  runner is still welcome for an agent with all three built-in groups on: the
  daemon enforces the rest either way. Otherwise its hello is refused and held
  for 30 s (closed at once, it would reconnect within a second), with one
  `runner_refused` notice in the chat per streak. A profile change while a v1
  runner is connected or refused recreates the sandbox instead of sending a
  `configure` it would ignore; the new container also picks up a rebuilt image.
  The hello reads the agent after registering and the change checks the hub
  after storing, so one always sees the other.
- **Subset rule:** a new agent's tools and connectors are at most its creator's;
  omitted means the creator's own. Checked in `agents::create` for every
  agent-created agent (approval card and Auto mode alike) and before a card is
  shown. The card lists `Tools:` and `Connectors:` and pins them, so the agent
  never gets more than the card showed, even if its creator changes meanwhile.
  The operator can set any profile (`POST`/`PATCH /v1/agents`).
- **Two edge cases, on purpose:** narrowing a creator later does not shrink the
  agents it already created; each profile stands alone and the operator narrows
  them one by one. A connector the operator approves through `add_connector` is
  given to the requester even when it is a sub-agent: a widening the operator
  allowed on the card.
- **Not done:** the drivers' prompt still names every ferrobot tool; a narrowed
  agent that calls a missing one is told it is turned off for it.

## Voice calls: words in the chat, addressed hand-offs, no spoken reasoning

- **The chat shows what was said.** Call messages are stored with the spoken
  words only; the call note and what an agent missed are kept in memory per
  (recipient, words) and swapped in when the message is handed to the runner
  (turn or steer). Stored before the message is queued, so a fast hand-off
  cannot miss it; a daemon restart ends the call and forgets them.
- **Only addressed agents answer.** An agent's line reaches another agent as a
  turn only when it names it (`@name`, stored as `<@id>`); the call note says to
  answer an agent only when asked and never to repeat. A guest does not get the
  host's announcement from the turn that invited it (it has the invite). The
  four-in-a-row cap stays. This stopped two agents restating themselves at each
  other (Tim's call, 2026-11-21).
- **Reasoning in the reply is held back.** Some OpenAI-compatible models
  (Tencent's hy4-flash via OpenRouter) stream their reasoning into `content`,
  ending with `</think…>`; the driver drops everything up to that tag, so it is
  neither shown nor spoken. An unclosed `<think>` is dropped; a bare
  "Thinking:" without a closing tag is kept (it may be the answer).

## Rebuilt runner images reach existing agents

- The stale check compared image names only, so a rebuilt `ferrobot-runner:dev`
  (same name, new id) never reached agents whose container already existed;
  they kept running the old runner (found when the reasoning filter did not
  apply, 2026-11-21). Now a container created from the base or a CLI image is
  stale when the image its name points to has another id; it is recreated on
  its next wake with its home volume kept. Snapshot and archive containers
  still never count as stale.
- `(pass)` is also recognised without brackets or with a full stop ("pass",
  "Pass."), in the daemon and the UI.

## Call rooms

- **A call with two or more agents moves into a room** (Tim, 2026-11-21): a
  group chat where the call happens, so the agents' own chats with the
  operator stay clean. A one-on-one call stays in the agent's chat. The room
  opens when a second agent joins (`invite_to_call` or `PUT /v1/call`), stays
  until the call ends even if only the host is left, and is kept as history.
  Its title is built from the members' current names ("Call · ann, bob"), so
  nothing but ids is stored; an agent deleted since drops out of the title.
- **Storage (v16):** `rooms` (created, ended), `room_members` (who joined,
  in order), `room_lines` (speaker `user` or `agent:<id>`, words, time), and
  `messages.room_id` for the mailbox messages that carry room turns. No agent
  foreign keys, so a room outlives deleted agents. A restart ends rooms left
  open (the call lives in memory).
- **Everyone reads, each decides.** Every line (the operator's words or an
  agent's reply) is queued for every other member and starts a turn there; the
  agent speaks or replies `(pass)`. The prompt is built when the turn starts,
  from everything that agent has not heard yet, so at most one room turn per
  agent waits at a time and a turn with nothing new is skipped without
  running. This replaces "answer only when named" inside rooms.
- **The cap stays at four agent lines in a row**, now counted in lines, not
  hand-offs; the operator's words reset it. Lines past the cap are still
  stored (they were spoken) but wake nobody; they reach the others with the
  operator's next words. Since each agent has at most one waiting turn, the
  turns already queued can add one more line each, so after the operator
  speaks there are at most 4 + (agents − 1) agent lines. Four kept: with two
  agents it allows two exchanges each, and the pass rule keeps bigger rooms
  shorter in practice.
- **A line is a completed room turn's text**: its `assistant_text` blocks
  (after the runner's think filter and mention normalising), joined, without
  `(pass)` blocks. Interrupted or failed turns make no line, and nothing of an
  agent's own-chat turns lands in the room.
- **Steering stays inside its place**: the operator's words steer only into a
  running turn of the same room (or, for chat messages, a chat turn).
- **Out of the agent's chat:** `GET /v1/agents/{id}/messages`, conversations
  between agents and unread counts leave room messages and room turns out
  (room turns are marked by `room_id` in their `message_in` event). The UI
  hides those turns' events and streaming text from the chat; the Activities
  tab still lists their tool calls. Question cards and shared
  files have no turn id and still show in the agent's chat, so a `create_agent`
  approval asked for in a call is not lost.
- **Call notes.** One note for every call message: spoken, short, everyone
  hears every line, speak when asked or when adding something new, otherwise
  `(pass)`, never repeat. The host also gets a host note: propose a specialist
  or deeper work in one sentence, wait for the operator's yes, then
  `create_agent` and `invite_to_call` it, or give it a background task;
  it says first that an approval may show in its chat (the card waits there,
  not in the room). An
  invited agent first hears that it should listen: at most a short hello or
  `(pass)`, then answer what it is asked. A room turn never gets the new-agent
  greeting. The operator's call message limit was 19,441 bytes (now 19,316,
  see "Call flow").
- **UI:** a "Calls" list in the sidebar (title and time), a room view with
  the transcript ("You" or the agent's avatar and name, time) that updates
  live, and a message box there while it is the running call's room. The call
  window's chat button opens the room once there is one. In an agent's chat
  during a room call, typed words go to that agent only.

## Call flow (Tim's room call test, 2026-11-21)

- **One agent at a time.** Room turns ran in parallel, so agents talked over
  each other ("one is asking if the other one got it but he is still
  talking"). A room now has a floor: one room turn runs at a time. A line
  makes the others due; they take the floor in order (the operator's words:
  everyone, host first, then join order; an agent's line: the others are
  appended) and each turn's prompt is built when it starts, so it hears the
  turn before it. Each still decides to speak or `(pass)`; the cap of four
  agent lines stays and the operator's words reset it. The operator speaking
  while an agent holds the floor queues as the next line, and in a room
  `POST /v1/call/messages` returns only the next speaker's message (none while
  someone speaks). An interrupted room turn means the operator cut in, so
  nobody else is due until they speak. Room messages jump the agent's queue
  (urgent). A member busy with its own chat work when the floor reaches it
  holds the room until that turn ends: accepted, a call member is rarely busy.
- **The host's announcement first.** When an agent invites another during a
  turn of its own, that turn holds the floor until it ends, so the guest's
  hello follows the host's words. A turn outside the room (the one-on-one
  turn that opens the room) holds the floor without becoming a line.
- **The call's end silences everyone else.** When the call ends (the host
  hangs up, or the operator), the room turn holding the floor is interrupted,
  queued room turns are settled unrun (`last_error` "the call ended") and no
  more lines are stored. The `call` event names the agent that hung up
  (`hung_up`); the UI lets that agent's goodbye finish and drops every other
  voice at once, queued and streaming.
- **Only the call is heard.** The UI speaks a turn only if it is part of the
  call, judged at its `message_in`: in a room call only room turns, in a
  one-on-one call the host's turns. A member's own chat work and a new
  agent's greeting stay silent. Tim saw "helper is speaking" with only
  team-lead on the card: the old rule spoke any turn of a member, and an
  agent that had left (likely helper hanging up after its hello) kept
  speaking off the card. The card now keeps a just-left agent's face while its
  goodbye plays.
- **No question cards in a call.** `ask_user` refuses while the agent is in a
  call ("ask out loud in your reply"); the call note says so. `create_agent`
  approval cards stay: they are the operator's safety gate.
- **Same language.** The call note says to speak the operator's language and
  names the voice settings' language code when set ("the operator's language
  (de)"), read when the call starts. The host note says to tell a new agent to
  speak this call's language. The operator's call message limit is now 19,316
  bytes (the longest note: a three-letter code plus the host note).
- A new agent invited into a call gets no greeting: its first turn there is a
  room turn, which never gets one; a greeting turn from a task stays silent in
  the call.

## Naming conventions and renaming

- **Settings, Agents, "Names for new agents"** (`agent_names` in the `general`
  setting, 1-300 characters, empty = short role names). The daemon appends it to
  the instructions it sends the runner (welcome and `configure`) of every agent
  with the "create agents" tool group, not to the stored instructions; a change
  reaches running agents at once with a `configure`. Names still have to pass
  the agent-name rules (lowercase `a-z0-9-`).
- **Rename** from an agent's right-click menu (`PATCH /v1/agents/{id}` with
  `name`); its computer and chat stay the same (`sandbox_name` does not change).

## A missing model key fails at once and is caught early

- **Setup problems are 400s.** The proxy answered a missing daemon key, a
  provider without a key or upstream, a failed `gcloud` token or an unusable
  provider with 503. The Claude SDK retries 5xx with backoff, so an agent went
  silent for about three minutes before saying "API Error: 503 … usually
  temporary" (Tim's call, 2026-11-21). These are now 400s with a plain message
  saying what to set up; internal errors stay 500.
- **No daemon-key agents without a key.** With the Claude driver and no
  `ANTHROPIC_API_KEY`, creating an agent or switching one to no provider is
  refused (400, "pick a model provider"). `/v1/health` gains `anthropic_key`;
  the agent dialog marks the default option as not set up, disables it and
  preselects the first provider for a new agent; the profile says so for an
  existing agent.

## Streaming speech in calls

- **Play while it streams.** The daemon already streamed ElevenLabs' audio, but
  the UI waited for the whole clip (`.blob()`). `GET /v1/voice/speech?text=…`
  (same logic as the `POST`, the operator token as `access_token` like other
  `GET` links) lets an audio element start with the first bytes; the speaker
  loads at most two clips ahead and aborts dropped ones. The Tauri CSP's
  `media-src` gains the daemon's address.
- **The first clause goes early.** A reply's first spoken chunk may end at a
  clause break (`,` `;` `:` or a spaced dash) once it has 20 characters, so
  speech starts before the first sentence is written; the rest still goes by
  sentence. Measured before (Tencent via OpenRouter): ~4.2 s model time per
  answer, mostly reasoning, which this does not change (Tim chose streaming
  only).

## Credential problems hold an agent's turns

- **Found in the proxy, per agent.** A missing daemon key (agent on the
  default), a provider without a key and an unusable `ChatGPT` sign-in mark the
  agent `missing`; an upstream 401 on a call that carried a credential marks it
  `rejected`. A 403 counts only on Bedrock, where AWS answers bad keys with it;
  elsewhere a 403 is moderation (OpenRouter), a missing permission (Anthropic)
  or a disabled API (Gemini). A failed `gcloud` token and a provider without an
  upstream stay plain 400s. The reason is a fixed sentence saying where to fix
  it, never the key or the upstream's body (Tim, 2026-11-21: a chat wall of
  identical "API Error: 400 the daemon has no Anthropic key").
- **In memory, set synchronously.** The hub holds `CredentialError {kind,
  reason, since}` before the proxy answers, so the failing turn already sees it
  when it settles; a restart forgets it and the next call finds it again. The
  agent view gains `credential_error` (null when fine) instead of a new
  `presence` value, so clients matching on presence keep working.
- **Turns wait, messages stay queued.** The dispatch loop waits while the
  problem stands; the turn that hit it ends once and its message is requeued
  without using up an attempt. The first report records one `credential_error`
  event (turn id in the payload, not on the event, so a room turn's notice still
  reaches the chat); the UI shows it instead of that turn's error. The Claude
  driver fails a turn only on an error result subtype, and the SDK writes a
  refused call as an "API Error: 400 …" answer (the bubbles in Tim's
  screenshot), so the turn may end *completed*. A completed turn that set the
  problem therefore counts as failed: no task report, the message is requeued,
  and the UI hides the answers written after the notice.
- **Cleared by anything that may fix it:** an agent PATCH of provider or model
  (after the computer is recreated, so the old runner cannot start a turn with
  the old settings), any provider PATCH (Vault saves keys through it) for the
  agents on it, a completed `ChatGPT` sign-in, and `POST /v1/agents/{id}/retry`
  for fixes made elsewhere (`gcloud auth login`, a key re-enabled upstream).
  Clearing wakes the agent when messages wait. A daemon key only arrives with a
  restart, which clears the state anyway.

## Webhooks (0.1.8)

- **Incoming hooks share the UI listener, outside `/v1`.** `POST
  /hooks/{id}/{secret}` is merged into the UI router after its auth and CORS
  layers, so the operator token neither guards nor opens it, and `/v1` is not
  widened (no entry in the middleware's open list). A separate opt-in listener
  was the alternative: a tunnel to its port could expose nothing else. It lost
  because the desktop app starts the bundled `ferrobotd` with fixed arguments,
  so a `--hooks-listen` flag would be out of reach for most users. Receiving
  from the internet therefore needs a tunnel or proxy that forwards only
  `/hooks/` (the guide shows a Cloudflare ingress rule with `path: ^/hooks/`).
- **The secret is in the path,** because GitHub, Zapier and most forms can
  only be given a URL, not a header. It is two v4 UUIDs (244 random bits, like
  agent tokens); only its SHA-256 is stored, compared in constant time. An
  unknown id and a wrong secret get the same `404`. Nothing logs request
  paths, and the URL is returned only by create and regenerate.
- **One JSON value in the `settings` table (`webhooks`), no migration.**
  Incoming hooks (with the secret's hash), outgoing hooks (without their URL)
  and a `next_id` counter that never reuses an id, so an old message never takes
  a new hook's name. Edits are serialized by a lock in `AppState`. Outgoing
  URLs are secrets (Discord and Slack put the token in them): secret store,
  `webhook:<id>`, deleted with the hook. Last deliveries and rate-limit counters
  are in memory, per daemon (not a static as in `otel.rs`, since tests run many
  daemons in one process).
- **A new `Sender::Webhook(id)` in the mailbox and `/v1`, but not on the
  runner wire.** Reusing `user` would show the payload as the operator's own
  words in the chat and treat it as such in calls; a routine sender would
  borrow a routine's name. Runners of 0.1.7 parse senders with a closed zod
  union and would reject the turn, and bumping the runner protocol would make
  `admit()` refuse every current runner with a restricted tool profile. So
  `turn` and `steer` frames carry `user`, as the daemon's greeting already
  does, with `sender_name` `webhook:<name>`, which is all the model sees. Old
  desktop clients show such a message like an unnamed routine's.
- **Framed once, when queued.** The stored body starts with `[webhook
  "<name>"]` and a sentence saying it is external data, not instructions; the
  payload comes last so it cannot close the frame. The turn, a steer and
  `read_mailbox` all see the same text. Bodies up to 64 KiB are accepted and
  the message is cut at the mailbox's 32 KiB.
- **Outgoing triggers sit where the facts are.** Question cards are caught in
  `AppState::publish`, which every card creation goes through. Replies (a
  completed turn on a message from the operator, a routine or a webhook,
  outside rooms) and final failures are sent from `settle()`; a task is done
  when the delegate's turn on the task message completes (`tasks::report`).
  Delivery is one background POST per hook with a 10 s timeout and no retry.
  Discord gets `allowed_mentions: {parse: []}` so agent text cannot ping
  `@everyone`.

## CLI logins through a token-swapping proxy (2026-11-26)

- **The proxy sits on the agent listener.** `CONNECT` is intercepted before
  routing (`egress::intercept`), so it is reachable exactly like `/model` and
  authenticates with the same agent token (proxy password, Basic). No new port.
- **Logins are generic, set up by the operator.** First built for GitHub only;
  before 0.1.11 shipped, Tim asked for any CLI that reads a token from a
  variable. A login is a name, its API hosts, its token variables, its programs,
  ask-first and its agents, in the `logins` setting; GitHub is a preset in the
  app. Agents cannot set up logins.
- **Swap only what was decided by the daemon, in request headers only.** The
  swap happens for the authenticated agent, a login lent to it and one of that
  login's hosts, and only where a request header carries that login's exact
  stand-in (`ferrobot-stand-in-<id>`, also inside `Basic` credentials, so git
  works). Never in the URL: a redirect or error page could hand it back.
  Response headers that repeat the token get the stand-in back. Redirects are
  passed back, never followed. Remaining risk: a service that echoes the token
  in a response body would show it to the agent.
- **The token goes only to public addresses.** A login's hosts are checked when
  saved (lowercase DNS names, no IP literal, no `localhost`, `.local`,
  `.internal`, `.lan`, `.home`) and resolved once per tunnel, every address
  public, with the client pinned to the checked address.
- **One name-constrained CA per login.** `rcgen` makes it (key and certificate
  in the secret store, `login:<id>:ca`); its permitted subtrees are the login's
  hosts, so even a leaked or misused CA vouches for no other site. Changed
  hosts make a new CA, and those agents restart anyway. One shared CA would
  have restarted every login agent whenever any login changed. Leaf
  certificates are made per host in memory and remade when their CA changes.
- **A login's variables never replace the sandbox's own.** Names the sandbox
  sets (proxy, certificate bundle, `PATH`, `HOME`...), `FERROBOT_…` and
  `GIT_CONFIG_…` are refused when saved, and provisioning only adds a login's
  variables where nothing is set. One host or variable belongs to one login.
- **Ask-first is a command-line heuristic.** It turns a safe shell command that
  runs one of the login's programs, or names one of its variables or hosts,
  into a risky one (a card unless Auto mode is on). A script the agent wrote can
  still reach the login; the boundary is the token never entering the sandbox.
- **Plain tunnels refuse private targets.** Port 443 only; a host that resolves
  to any loopback, private, link-local, CGNAT or unique-local address is
  refused, and the tunnel connects to the address it checked. Without this a
  sandbox could reach the operator API on loopback through the daemon.
- **Environment only for agents with a login.** `HTTPS_PROXY`, the stand-ins,
  git's credential helper for a login with `github.com` (`GIT_CONFIG_COUNT`, git
  2.31+) and the CA bundle (`SSL_CERT_FILE`, `GIT_SSL_CAINFO`, `CURL_CA_BUNDLE`,
  `REQUESTS_CA_BUNDLE`, `NODE_EXTRA_CA_CERTS`) are set at provisioning; changing
  a login's hosts, variables or agents recreates those computers (home kept),
  as a model switch does.
- **Dependencies.** `rcgen` 0.14.10 (`ring` backend, already in the tree) brings
  `rcgen` and `yasna`; the lockfile also lists `x509-parser` and its parsers for
  an optional `rcgen` feature that is not built. `tokio-rustls`, `hyper`,
  `hyper-util` and `http-body-util` were already in the tree.

## Add CLI (2026-11-26)

- **One list of CLIs, one dialog.** Tim asked for "CLI → Add CLI" instead of a login set up
  in one place and a token in another. A CLI (`ferrobot_core::Cli`) is its install, its programs,
  an optional account and its agents, all in the `clis` setting; the earlier `logins` setting and
  `/v1/logins` never shipped and are gone. `logins::all` derives today's logins from CLIs with an
  account, so the proxy, the sandbox environment and the ask-first check did not change.
  The package list of before is read once into Debian CLIs, and `PUT /v1/clis {packages}` keeps
  working (additive API).
- **Debian into the image, everything else into homes (Tim's option D).** A new CLI in the image
  means a build and a restart for every agent, which Tim did not want for every tool. npm,
  download and script installs go into `~/.local` of the picked agents (`PATH` puts
  `~/.local/bin` first, set at provisioning; every agent is recreated anyway when the runner image
  changes), once per agent: a marker file per CLI holds the install it ran, so a change reinstalls
  and a removal can uninstall (npm, download; a script's files stay). It runs on provisioning, on
  a change for running agents, and on the next wake for the others (a pending set, filled again
  at startup) rather than on every wake. Root exec does not exist, so no symlinks into `/usr`.
- **The CLI's own device sign-in, in a throwaway sandbox.** Reading a CLI's token from its config
  needs knowing where each CLI keeps it; its token command (`gh auth token`) does not. The login
  runs in the background with output to a file (`gh` 2.23 prints its code without a TTY), the daemon
  polls it for the address and code, then runs the token command and stores the result. The
  sandbox (`signin.<id>`, a name no agent can have; `sleep infinity` instead of the runner) has no
  agent token, no proxy and no mounts, is removed afterwards or on cancel, and leftovers are
  removed at startup. Remaining risk, told to Tim: the CLI, and whatever its install ran, sees the
  token during its own sign-in, as when run on the operator's computer. Device sign-ins only;
  callback-to-localhost sign-ins would need a forwarded port.
- **Agents propose, the operator installs.** `propose_cli` (Add connectors group) shows the exact
  install, "no checksum" for a download without one, and the token hosts; approving adds the CLI
  for the proposer without a token. Auto mode never skips it, because the card decides what runs in
  the computers and where a token would go.
- **CLI images per daemon.** The image sweep removed every `ferrobot-runner:clis-*` image on the
  engine, so a second daemon (a test copy) could remove the operator's image. Tags now carry the
  daemon's data dir hash (`clis-<owner>-<hash>`) and the sweep only removes its own; images from
  before are no longer swept.


## Security review before 0.1.11 (2026-11-27)

A review of the release before it shipped changed these decisions:

- **Proposed CLIs carry no sign-in commands.** They run in the container where the real token is
  made and were not on the card, so an agent could have chosen commands that send the token out.
  An agent proposes the install, programs, hosts and variables (asking first by default); the
  operator adds sign-in commands in Edit, and the sign-in dialog shows them before it starts.
- **A late CLI card adds nothing.** Approving re-reads the CLIs and refuses (409) when a CLI of that
  name exists by then, so it can never take over the operator's CLI and its token.
- **The token is swapped only in credential headers** (`authorization` including Basic,
  `x-api-key`, `private-token`, `x-auth-token`). Any header was a way to get it back from a service
  that repeats a header such as `User-Agent` in its answer. Response headers are still scrubbed
  everywhere; a response body that echoes a credential header stays the known remaining risk.
- **No CLI account for Codex agents.** Codex runs its shell itself, so its commands never reach
  `tool_check`; with a token behind the proxy that would let critical steps run without a card.
  Same mechanism as the Desktop refusal, checked both when lending an account and when switching
  an agent's provider.
- **Home programs last on `PATH`.** Prepending `~/.local/bin` let a program an agent once planted
  stand in for the runner's `node` after its Shell was taken away, or for the screen's `x11vnc`.
  The daemon also calls the desktop's programs by full path, and the desktop script sets a
  system-only `PATH` (the dock starts the Chromium wrapper by its full path).
- **The knowledge base shows authors and one line per entry.** Agents' entries now reach every
  memory agent's instructions, so each says who wrote it, control characters fold to spaces (an
  entry cannot open a section of its own), keys must be one line, and all of the operator's
  entries come first, outside the limit of 50 agent entries.
- **The tool check reads words, not spellings.** Connector tool names split at separators and
  where lower case turns upper (`CREATE_REFUND`, `createRefund`), and plurals match their stem.
  Tools that run arbitrary input (SQL, HTTP, GraphQL) are Risky. git and gh skip option values
  (`-C dir`, `-R o/r`) to find the subcommand; `gh api` takes its method from every spelling and
  POST from fields. Still best effort: it reads the command line, never a script the agent wrote.
- **Cards show everything they approve or refuse to exist.** A shell command over 4,000
  characters gets no card at all (refused) rather than a cut one; card text with line breaks,
  bidi or invisible characters in a connector's command, URL or reason is refused.
- **Agents reach the host's localhost on Docker Desktop.** Checked: a container reached
  `host.docker.internal:7878` (`/v1/health` 200, `/v1/agents` 401). Every UI route but health,
  the OAuth callback and signed artifacts needs the operator token, so the risk is other local
  services without auth (model servers, dev databases, debug ports) and the LAN. Not fixed now:
  a real fix is an internal network whose only route out is the agent listener; the wiki tells
  operators not to run unauthenticated services on this computer while agents run.
- **Harness settings folders are off limits.** Gemini trusts the workspace
  (`GEMINI_CLI_TRUST_WORKSPACE`) and Claude and Codex read project settings, all of which can start
  processes; the runner refuses writes under `.claude`, `.codex` and `.gemini` for every driver
  instead of dropping the trust flag, which was not verified to keep Gemini working.
- **Vault notes follow the agent, not its name.** A deleted agent's folder moves to
  `ferrobot/.archived/<name>-<id>` (Obsidian hides dot folders); creating a name archives any
  folder left under it, and a rename archives the target first instead of merging.

## Skills from skills.sh (2026-11-27)

- **Agents find and install skills themselves, through the shell.** Tim's idea: an agent with a
  task it has no knowledge for looks up a skill on skills.sh (`npx -y skills@1.9.1 find`), installs
  it into its home and follows its `SKILL.md`. Going through the shell instead of a harness's own
  skill tool means it works for every driver, including Claude Code on the host, whose built-in
  `Skill` tool stays off.
- **Install asks, looking does not.** `skills add`, `update` and the experimental install commands
  are Risky in `tool_check` (a card unless Auto mode is on); `find`, `list` and `use` are safe.
- **Pinned and framed.** The CLI version is fixed in the instructions (`SKILLS_CLI`), and a skill is
  described to the agent as third-party content that never overrides its instructions. Remaining
  risk: a skill is someone else's instructions and scripts, like a web page the agent acts on; keys
  and CLI tokens are not in the sandbox, and its commands meet the same checks.
- **Only agents with Shell** get the note: without a shell they could not run the CLI or a
  skill's scripts.
- **find-skills ships with ferrobot.** Tim wanted every agent to start with it. It is one MIT
  `SKILL.md` (vercel-labs/skills at e7fd0e9), vendored under `crates/daemon/src/skills/` with its
  licence and written into `~/.agents/skills/find-skills/` when a computer is made or wakes without
  this version (marker `.ferrobot-defaults`). Vendoring instead of `npx skills add` at provision:
  no network at provision, no upstream change reaching agents unseen.

## Tasks with a goal and slash commands (2026-11-27)

- **The agent says when it is done, and a model checks.** Tim wanted `/task` to work like a goal
  that holds across turns, for every provider. The daemon's turn-end hook (`runner_ws::settle`)
  sends the agent back to work until it calls `finish_task` and ends a turn with a report; then a
  check reads the goal, the summary, the turn's steps and the report and answers `{met, reason}`.
  Hooks inside one harness would have covered Claude only.
- **The check uses what the agent already uses.** Claude Code: Haiku through the host's own
  `claude -p` (no tools, settings, MCP servers or session; the prompt on stdin). Anthropic and the
  daemon's own key: the Messages API. OpenAI-compatible kinds (OpenAI, Gemini, OpenRouter, Ollama,
  any other): chat completions. The agent's model, else the provider's default; the key stays in
  the daemon. An API check is a Usage row under the turn `task-check:<turn>`, so it stays out of the
  turn's trace; the Claude Code check records none. Codex, Gemini CLI, Bedrock and Vertex get no check (their APIs need code the
  daemon only has as a proxy), and neither does an agent without a model: `finish_task` with a
  report ends the task there. A per-provider `judge_model` was skipped: it would need a migration.
- **A card or a teammate holds the task.** A turn that ends without `finish_task` is not sent back
  while it left an unanswered card of its own or a task it gave (`assign_task`) is still queued or
  worked on: the operator's answer or the teammate's report starts the next turn, and turns on
  agents' messages run the hook too. Work handed out with `send_message` holds nothing, so the task
  note asks for `assign_task`.
- **The task's agent decides its teammates' risky steps (2026-11-27).** Tim wanted a task to run
  without him answering every teammate's card. A risky (never a critical) tool-call card of an
  agent in the task agent's chain (`stop::chain`) also goes to the task agent as a note, and it
  answers with `decide_approval` through the operator's answer path, recorded as `answered_by`.
  The operator's card stays open, so the first answer wins (`409` after). The task agent never
  decides its own steps, which would be Auto mode nobody switched on, nor any other kind of card.
  This is narrower than Auto mode, which skips those cards entirely, but not by much: it can hand
  its own step to a teammate and approve it there, and a teammate's command text reaches it. The
  notes are kept on the task (`notes`) and dropped when the card is decided or the task ends.
- **A check that cannot run counts as not met** and uses up a continuation, so a broken check ends
  in "gave up" rather than a false "achieved".
- **State in the `goals` setting**, one entry per agent, so a task survives a restart without a
  migration. The continuation is the operator's message (it starts a turn on every driver) and is
  hidden in the chat behind a `goal` event, like reaction notes. Stop and Clear drop a waiting
  continuation; Stop also ends the task while the agent is idle.
- **Slash commands live in the app.** The daemon keeps the list (`commands` setting, empty at
  first: `/task` is the only built-in) and the app expands it, so the daemon has no second message
  path. `task` is reserved.
- **Agent-made commands always need a card.** A command's prompt is later sent in the operator's
  name, so `add_command` shows the full prompt and Auto mode never skips it; the card stays out of
  Discord. A name taken while the card waits blocks Approve with `409` and leaves the card open.
