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
