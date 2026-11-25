# Plan: ferrobot in the cloud

Status: proposal, 2026-11-25. Nothing here is built yet. Decisions marked
**Decide** need Tim's call before the phase starts.

## Goal

ferrobot runs 24/7 on a server: the daemon, the agents' computers and the UI,
reachable from a laptop or phone anywhere, without weakening any rule in
`CLAUDE.md` ("Must not break"). The desktop app keeps working as today.

Not in scope: several operators per daemon (multi-tenant), a hosted ferrobot
service, mobile apps.

## Target picture

```
 browser / desktop app / Discord
          │  HTTPS (private network or authenticated edge)
          ▼
 ┌──────────────────────────── control plane (always on) ──────────────┐
 │ ferrobotd: /v1 API + web UI, scheduler, model proxy, MCP, bridges    │
 │ SQLite on a volume · secrets in a pluggable store · archives → S3    │
 └───────────────▲─────────────────────────────────────────────────────┘
                 │ agent API over TLS (runner WebSocket, /mcp, /model),
                 │ per-agent tokens; the sandbox dials out, nothing dials in
 ┌───────────────┴──────────── sandboxes (pluggable backend) ──────────┐
 │ docker (same host) │ lambda-microvm (AWS) │ firecracker (KVM host)   │
 │ later: e2b / sprites, through the same guest channel                 │
 └─────────────────────────────────────────────────────────────────────┘
```

The control plane must be a long-running host (a VM or a container with a
volume). It cannot itself run on Lambda MicroVMs: their total lifetime is
capped at 8 hours.

## What works today

- `ferrobotd --backend docker` runs on Linux; containers reach the daemon
  through `host.docker.internal` (`host-gateway`, `docker.rs`).
- The UI is a static web app that talks only to `/v1` and `/v1/ws`, and has a
  connect screen for the daemon URL and token.
- The Discord bridge and outgoing webhooks only call out; archives can go to
  S3 or R2 (`--storage-url`).

So a server setup works now with manual steps (build on the server, Tailscale
in front, `--secret-store file`). Its gaps are below.

## Phase 1: a server-ready daemon (about 2 days)

1. **Encrypted secret store** (`--secret-store sealed`, the default when no
   keychain is reachable). One file, AES-256-GCM with the existing `seal`
   code, a fresh nonce per write. The 32-byte master key never sits next to
   the file; it comes from, in order:
   - `$CREDENTIALS_DIRECTORY/ferrobot-master-key` (systemd `LoadCredentialEncrypted=`,
     sealed to the host's TPM2 where there is one),
   - `FERROBOT_MASTER_KEY_FILE` (a file outside the data dir, mode 0600),
   - AWS KMS (envelope: the file stores a data key encrypted by a KMS key;
     the daemon decrypts it once at startup with its instance role).
   `--secret-store file` (plaintext) stays for CI only. The archive key
   (`storage::archive_key`) lives in this store, so remote archives work on a
   headless host. **Decide:** KMS now or later (it needs an AWS SDK crate,
   see Dependencies).
2. **The daemon serves the web UI** (`--ui-dir`, or the built `dist/`
   embedded at compile time as the Tauri shell does) and accepts its own
   origin in CORS. One URL for UI, `/v1` and the WebSocket; no reverse-proxy
   rules per path.
3. **Server packaging:** a `ferrobotd` container image (`linux/amd64`,
   `linux/arm64`) and a Linux binary in the release, a `deploy/compose.yml`
   (daemon + Docker socket, volume for the data dir) and a systemd unit with
   `LoadCredentialEncrypted=`.
4. **Safe defaults on a server:** the UI listener stays on loopback; the agent
   listener binds the Docker bridge (`172.17.0.1`), never `0.0.0.0`;
   `/v1/health` reports which secret store is in use.
5. **Wiki page "Run on a server":** VM size (4 vCPU, 8 GB, 80 GB disk for
   about five awake agents), Docker, Tailscale (`tailscale serve` for HTTPS,
   which voice needs; `tailscale funnel` only for `/hooks/`), backups of the
   data dir, `claude` and `codex` logins on the server for those providers.

Tests: the sealed store round-trips, refuses a wrong key, and never writes
plaintext (unit tests); the daemon serves `index.html` and the API on one
origin (feature test); the compose file starts and answers `/v1/health`
(manual, noted in the PR).

## Phase 2: a guest channel for remote sandboxes (about 2 days)

Docker gives the daemon `exec`, file access and `docker commit` for free.
Remote sandboxes do not, and Lambda MicroVMs offer no exec or file API at
all. So the runner, which already holds a WebSocket to the daemon, carries
them:

- New frames: `DaemonFrame::Exec { id, argv, cwd, env, stdin, timeout }`,
  `ReadFile`, `WriteFile`, `PackHome`/`UnpackHome` (a tar of `$HOME`, streamed
  in chunks); `RunnerFrame` answers for each. Additive, so `/runner/ws`
  stays compatible.
- A `GuestChannel` helper in `crates/daemon` that a backend uses for
  `exec`, `read_file`, `write_file`, `snapshot` and `restore`.
- Archives keep their current format: `PackHome` output goes through
  `seal` into the object store, so a home moves between backends.
- The shell gate (`tool_check.rs`, `runner/src/gate.ts`) applies to agent
  commands, not to these daemon-initiated operations, which never come from
  the model.
- The agent API must be reachable from outside the host: TLS in front of
  `--agent-listen` (Caddy, an AWS load balancer, or a VPC-private endpoint).
  Per-agent tokens already guard `/runner/ws`, `/mcp`, `/model` and
  `/host/claude-code/turn`; add a rate limit per token on `/model`.

Tests: the Docker backend gets a mode that uses the guest channel instead of
`docker exec`, so the whole channel runs in `docker-tests` and e2e without a
cloud account.

## Phase 3: AWS Lambda MicroVMs backend (about 4 days)

Lambda MicroVMs (2026) are Firecracker microVMs built from a Dockerfile, with
launch, suspend, resume and terminate, memory and disk kept while suspended,
and a total lifetime (running plus suspended) of at most 28,800 s (8 hours).

How `SandboxBackend` maps onto it:

| ferrobot | Lambda MicroVMs |
| --- | --- |
| `create` / cold `wake` | `run-microvm` from the ferrobot image, `NO_INGRESS`, egress to the internet or the daemon's VPC; the runner dials the daemon; `UnpackHome` from the agent's archive |
| warm `sleep` / `wake` | `suspend-microvm` / `resume-microvm` (the daemon resumes explicitly; with `NO_INGRESS` nothing else can) |
| cold `sleep` | `PackHome` to the archive, then `terminate-microvm` |
| `exec`, files, `snapshot`, `restore` | the guest channel (phase 2) |
| `state` | `list-microvms` plus the runner connection |
| `destroy` | `terminate-microvm`, delete the archive |

- **No AWS idle policy:** Lambda's `maxIdleDurationSeconds` counts only
  inbound traffic, and with `NO_INGRESS` there is none, so it would suspend a
  MicroVM in the middle of a turn. Leave the idle policy out (or at its
  28,800 s maximum) and let the supervisor suspend: it knows when a turn,
  its tool calls and queued messages are done (OpenAI's lifecycle guide: an
  idle event alone is not a safe shutdown signal). Keep the current warm
  window (`--warm-after`, 300 s), the common default for agent workloads.
- **The 8-hour cap:** the backend tracks each MicroVM's start time and does a
  cold sleep (pack, terminate) before 8 hours, at the next idle moment. The
  next message launches a new MicroVM and restores the home, so the agent's
  computer survives, only its running processes do not.
- **Image:** the runner Dockerfile, zipped to S3, turned into a MicroVM image
  once per release and region (hooks `/run`, `/suspend`, `/resume`,
  `/terminate` on a port that `allowedPorts` excludes; `/run` starts the
  runner). **Decide:** we publish images per region, or the daemon builds the
  image in the operator's account at setup (simpler to ship, slower first run).
- **Credentials:** the daemon runs with an IAM role (EC2 instance profile or
  ECS task role) allowed to call `lambda-microvms` on images tagged
  `ferrobot`, and to read and write the archive bucket. No AWS key in a
  sandbox; the MicroVM's own execution role gets no permissions.
- **Keys stay in the daemon:** the runner reaches the model only through
  `/model`, as today, so the MicroVM never sees a provider key.
- **Cost** (us-east-1 prices from the pricing page, to recheck): 1 vCPU and
  2 GB cost about $0.13 per running hour; suspended costs only snapshot
  storage ($0.08 per GB-month) plus $0.0038 per GB written at suspend and
  $0.00155 per GB read at resume. An agent busy 2 hours a day with 20
  suspends comes to roughly $8 compute plus $6 snapshots a month. A single
  4 vCPU VM running Docker for all agents is cheaper for one operator; the
  MicroVM backend buys a VM boundary per agent and nothing running while idle.
- **Availability:** announced 2026-06-22 as "available", with no access form
  in the announcements; 10 regions since 2026-08-19, among them Europe
  (Frankfurt), Europe (Ireland) and Europe (Stockholm). The memory of running
  plus suspended MicroVMs has a per-region quota (Service Quotas, "Lambda
  MicroVMs").
- **Unknowns to check first** (a half-day spike): whether a fresh account
  can launch right away (the OpenAI cookbook says it needs "Lambda MicroVMs
  access"), per-MicroVM vCPU, memory and disk limits, the default quota,
  whether a WebSocket held from inside survives `suspend`/`resume` (the
  runner must reconnect if not), image size limits.

## Phase 4: Firecracker on EC2 (finishes roadmap item 1)

Since February 2026, EC2 offers nested virtualization on regular instances
(C8i, M8i, R8i and others, Intel only, no extra charge), so the existing
Firecracker backend can boot on an ordinary EC2 VM instead of bare metal.
Its `TODO(kvm)` items (process spawn, TAP, snapshots) get a real host, and
the vsock guest agent can be replaced by the phase 2 guest channel, which
removes one component. No 8-hour cap, no per-agent bill, real VM isolation,
but we operate the host.

## Later: other sandbox providers

The guest channel makes a backend mostly lifecycle calls. Candidates: E2B
(pause keeps memory and processes, about 1 s resume, run time capped per
session), Fly Sprites (persistent disk, no duration cap, billing stops when
idle; our `sprites` backend is a stub today). Vercel Sandbox and Modal keep
only the filesystem or snapshots that expire, a weaker fit for long-lived
agent computers.

## Secrets: why not our own vault service

The daemon needs plaintext keys at the moment it calls a provider, a
connector, Discord or a webhook. A separate "vault" process that only stores
secrets and hands them to the daemon on request adds a network hop and an
attack surface, but protects nothing: whoever controls the daemon can ask it
for every secret. Mature stores already do the storing:

| Store | Fits | Cost and effort |
| --- | --- | --- |
| OS keychain (today) | desktop | none |
| Sealed file + systemd credential / key file (phase 1) | any single server | small, no new service |
| Sealed file + AWS KMS envelope | servers on AWS | a few cents a month, one SDK crate |
| AWS Secrets Manager | AWS, few secrets | $0.40 per secret a month plus API calls; ferrobot holds 5 to 30 |
| OpenBao / Vault (KV or transit) | teams that already run it | a service to run, unseal and back up |

Recommendation: `SecretStore` becomes a trait with `keyring`, `sealed` and
`file` in phase 1, and an optional `aws-secrets-manager` or `openbao` store
only when someone needs it.

**Where a separate process does pay off** is if it never hands keys out: a
small credential proxy that holds the keys and adds them to outbound
requests itself, for an allow list of hosts. Then the daemon, which agents
talk to all day, holds no keys at all. That is the same machinery as the
token-swapping proxy for CLI logins (roadmap item 8). If we build that, it is
worth building once and moving the model proxy's key swap, connector tokens
and the Discord token behind it. Not before phase 3.

## Security checklist for every phase

- No key, token or master key in a sandbox, env, log, event or API response;
  the sealed store's key never in the data dir.
- The UI listener never public without a second factor in front (Tailscale,
  or an identity-aware proxy); `/hooks/` is the only route meant for the
  internet.
- The agent API on the internet only with TLS, per-agent tokens and a rate
  limit; prefer a VPC-private path for cloud sandboxes.
- Remote archives always encrypted (`archive_key`), as today.
- Cloud credentials through instance roles, never stored in ferrobot.

## Dependencies (ask before adding)

- Phase 1: none (`ring`/`aws-lc` for AES-GCM and `object_store` are in the
  tree); KMS would add `aws-config` and `aws-sdk-kms`.
- Phase 3: SigV4 signing for the Lambda MicroVMs API: `aws-sigv4` and
  `aws-credential-types` (small), or the full `aws-sdk-lambda` if it covers
  `lambda-microvms`. **Decide** after the spike.

## Order and decisions

1. Phase 1, then try it on a cheap VM with Tailscale for a week.
2. Phase 2 (tested with Docker, no cloud account needed).
3. The half-day Lambda MicroVMs spike; then phase 3 or phase 4 first,
   depending on access, limits and cost.

**Decide:** start with phase 1? KMS in phase 1 or later? Images per region or
built in the operator's account?

## Sources

- AWS Lambda MicroVMs: <https://aws.amazon.com/lambda/lambda-microvms/>,
  guide <https://docs.aws.amazon.com/lambda/latest/dg/lambda-microvms-guide.html>,
  launching <https://docs.aws.amazon.com/lambda/latest/dg/microvms-launching.html>,
  pricing <https://aws.amazon.com/lambda/pricing/>
- OpenAI cookbook, sandboxes on Lambda MicroVMs:
  <https://developers.openai.com/cookbook/examples/agents_api/sandboxes/aws/readme>
- Bedrock AgentCore Runtime sessions (microVM per session, state ends with
  the session): <https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/runtime-sessions.html>
- EC2 nested virtualization:
  <https://aws.amazon.com/about-aws/whats-new/2026/02/amazon-ec2-nested-virtualization-on-virtual>,
  <https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/amazon-ec2-nested-virtualization.html>
- Sandbox providers compared: <https://fly.io/learn/sprites-vs-e2b-vs-modal-vs-daytona/>,
  <https://upstash.com/blog/ai-agent-sandbox-providers-compared-2026>
- Secret stores: <https://infisical.com/blog/openbao-alternatives>
