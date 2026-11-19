# ferrobot protocol (v1)

Machine-readable contracts for everything that talks to `ferrobotd`. They are
**generated from the daemon's Rust types** (`utoipa` derives in
`crates/daemon/src/spec.rs`). Do not edit them by hand.

| File | What it describes |
| --- | --- |
| [`openapi.json`](openapi.json) | OpenAPI 3.1: the operator HTTP API under `/v1` (UI listener, default `127.0.0.1:7878`). |
| [`ui-events.schema.json`](ui-events.schema.json) | JSON Schema 2020-12: one text frame on `GET /v1/ws` (`UiEvent`). |
| [`runner-protocol.schema.json`](runner-protocol.schema.json) | JSON Schema 2020-12: daemon ↔ runner frames on `/runner/ws` (agent listener). |
| [`fixtures/`](fixtures) | Example frames, parsed by both the Rust and the TypeScript test suites. |

## How they stay in sync

- `cargo test -p ferrobot-daemon --test specs` fails if a committed file differs
  from what the Rust types generate. Regenerate with
  `FERROBOT_UPDATE_SPECS=1 cargo test -p ferrobot-daemon --test specs`.
- The same test validates every fixture against `runner-protocol.schema.json`
  and sample `UiEvent`s against `ui-events.schema.json`.
- The daemon's integration and e2e tests check **every** `/v1` response they
  receive against `openapi.json` (status documented, body matches schema).
- The desktop app's TypeScript types are generated from `openapi.json`
  (`npm run gen:api` in `apps/desktop`); CI fails if they are stale.

## Authentication

All operations except `GET /v1/health` need the operator token:
`Authorization: Bearer <token>` (OpenAPI scheme `bearer`), or on `GET`
requests only, `?access_token=<token>` (scheme `access_token`, for WebSocket
upgrades and images). Wrong or missing tokens get `401` with
`{"error": "unauthorized"}` and `WWW-Authenticate: Bearer`. The daemon keeps
the token in `<data dir>/api-token` (mode 0600) unless `--api-token` sets one.

## What is in v1

**HTTP** (`openapi.json`, tags `system`, `agents`, `chat`, `memory`,
`routines`, `sandbox`): health; agents with profile edits including renames (`PATCH`, `409` if
the name is taken), deletion (`DELETE` purges disk and snapshots unless
`?keep_data=true`), avatars
(`PUT/GET/DELETE …/avatar`, raw PNG/JPEG/WebP/GIF body ≤ 512 KiB) and
`POST …/read`; conversation and events; interrupt, stop, wake, exec; sandbox
status; snapshots with restore; memory (`?q=` searches); routines
(`/v1/agents/{id}/routines`, `/v1/routines/{rid}`, `…/run`, `…/runs`).

**`UiEvent`** (`GET /v1/ws`, field `type`):

| `type` | Meaning |
| --- | --- |
| `agent` | Full `AgentView` after any change: profile, `presence` (`working`, `awake`, `starting`, `sleeping`, `error`), `connected`, `unread`. |
| `agent_deleted` | The agent is gone. |
| `message` | A mailbox message to or from the agent (`sender`: user, agent or routine). |
| `event` | A stored turn event (incoming message, text, tool use, tool result, lifecycle). |
| `delta` | Streaming assistant text for the running turn; not stored. The final `text` event replaces it. |
| `memory_changed` | The agent's memory changed; refetch. |
| `routines_changed` | The agent's routines or their runs changed; refetch. |

**Runner frames** (`/runner/ws`): runner → daemon `hello`, `event`,
`turn_done`; daemon → runner `welcome` (includes the instructions), `turn`,
`interrupt`, and `configure` (current `agent_name` and `instructions` after a
rename or instructions edit; they apply from the next turn). Turn events (field `kind`) include `assistant_delta`
(streamed text, not stored), `assistant_text`, `tool_use` with an `id`, and
`tool_result` referencing that id. A turn's `sender` is `user`,
`agent:<id>` or `routine:<id>` (the `sender_name` of a routine turn is
`routine:<name>`).

## Versioning

- HTTP paths are prefixed with `/v1`. Breaking changes get a new prefix; adding
  optional fields or new endpoints does not.
- The runner protocol carries `protocol_version` in `hello`/`welcome`
  (`ferrobot_core::PROTOCOL_VERSION`).
- Clients should ignore unknown fields and unknown `type` values in `UiEvent`.

## Writing another client

A native app (e.g. SwiftUI) needs only these files: generate models from
`openapi.json` (for example with `swift-openapi-generator`), open `GET /v1/ws`
for live updates and refetch via HTTP after a reconnect. Errors are always
`{"error": "<code>", "message": "..."}` (`ErrorBody`).
