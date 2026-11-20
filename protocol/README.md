# ferrobot protocol (v1)

Machine-readable contracts for everything that talks to `ferrobotd`. They are
**generated from the daemon's Rust types** (`utoipa` derives in
`crates/daemon/src/spec.rs`). Do not edit them by hand.

| File | What it describes |
| --- | --- |
| [`openapi.json`](openapi.json) | OpenAPI 3.1: the operator HTTP API under `/v1` (UI listener, default `127.0.0.1:7878`). |
| [`ui-events.schema.json`](ui-events.schema.json) | JSON Schema 2020-12: one text frame on `GET /v1/ws` (`UiEvent`). |
| [`runner-protocol.schema.json`](runner-protocol.schema.json) | JSON Schema 2020-12: daemon ↔ runner frames on `/runner/ws` (agent listener). |
| [`question-card.v1.schema.json`](question-card.v1.schema.json) | JSON Schema 2020-12: question cards (version 1), the payloads of `question` and `question_answered` events. |
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
`routines`, `sandbox`, `providers`, `connectors`): health; agents with profile edits including renames (`PATCH`, `409` if
the name is taken), deletion (`DELETE` purges disk and snapshots unless
`?keep_data=true`), the character avatar look (`character`:
`{body, eyes, hue}`, `null` = derived from the id; `PATCH` with `null`
resets it), avatar images
(`PUT/GET/DELETE …/avatar`, raw PNG/JPEG/WebP/GIF body ≤ 512 KiB) and
`POST …/read`; conversation and events; interrupt, stop (`?level=warm`
pauses where the backend can, `501` otherwise; default `cold`), wake, exec;
sandbox status; snapshots with restore; memory (`?q=` searches); routines
(`/v1/agents/{id}/routines`, `/v1/routines/{rid}`, `…/run`, `…/runs`).
Agent `status` is `created`, `running`, `paused` (warm sleep), `stopped`,
`archived` or `failed`; `provider_id` and `model` pick the model (`null`: the daemon's
`ANTHROPIC_API_KEY` / the provider's default model), and changing them
rebuilds the sandbox with its disk kept.

Model providers: `GET/POST /v1/providers`, `PATCH/DELETE /v1/providers/{id}`
(`409` while agents use it). `api_key` is write-only; responses carry
`has_key`. `codex` providers can sign in with ChatGPT on the daemon host:
`POST /v1/providers/{id}/chatgpt-login` returns a device code,
`GET` on the same path reports `none`, `pending`, `signed_in` or `failed`.

Agent-to-agent threads: `GET /v1/conversations` (one entry per pair, most
recent first) and `GET /v1/conversations/{a}/{b}` (both directions, oldest
first, read-only).

**Mentions:** message bodies and `assistant_text` events store mentions of
existing agents as `<@agent-id>` tokens; render them with the agent's current
name. `MailMessage.priority` is `true` (urgent), `false` (deferred, does not
wake the recipient) or `null`.

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

**Runner frames** (`/runner/ws`): runner → daemon `hello` (`driver` is
`mock`, `claude`, `openai`, `codex` or `acp`), `event`,
`turn_done`; daemon → runner `welcome` (includes the instructions), `turn`,
`interrupt`, and `configure` (current `agent_name` and `instructions` after a
rename or instructions edit; they apply from the next turn). Turn events (field `kind`) include `assistant_delta`
(streamed text, not stored), `assistant_text`, `tool_use` with an `id`, and
`tool_result` referencing that id. A turn's `sender` is `user`,
`agent:<id>` or `routine:<id>` (the `sender_name` of a routine turn is
`routine:<name>`).

## Question cards (agent → UI)

Agents show their operator structured questions with the MCP tool
**`ask_user`** (input: `QuestionCard`). A card holds 1–8 questions, shown one
after another and answered together. Each question is `single` (pick one),
`multi` (pick any) or `text` (free text); `single`/`multi` take 2–12
`choices` (`{label, value}`) and may set `allow_custom` for an extra free-text
answer. The tool does not block: it records a **`question` event**
(`QuestionEvent`, `version: 1`) in the agent's chat (pushed as `UiEvent`
`event`) and returns its id. The operator answers with
`POST /v1/agents/{id}/questions/{qid}/answer` (`{"answers": [QuestionAnswer]}`;
`400` if the answers do not fit the card, `409` if already answered). The
daemon then queues the answers to the agent as one `user` message and records
a **`question_answered` event** (`QuestionAnswered`: `question`, `answers`,
`message_id`), which locks the card. UIs show the answers on the card and may
hide that message from the chat.

**Approval cards.** When an agent calls **`create_agent`** (`name`,
`description`, optional `instructions` and `character`), the daemon checks the
name and the agent limit (`FERROBOT_MAX_AGENTS`, default 10; also enforced on
`POST /v1/agents`, `409`), then records a `question` event whose
`QuestionEvent.approval` is set (`{"kind": "create_agent", name,
description}`) and whose single question offers the choices `approve` and
`decline`. The tool call waits for the answer. The answer endpoint hands the
decision to that call instead of messaging the agent (`message_id` is
`null`); approve creates the agent with `created_by` set to the requester and
the tool returns `{"status": "created", "agent_id": ...}`, decline returns
`{"status": "declined"}`. Without an answer within `FERROBOT_APPROVAL_TIMEOUT`
(default 300 s) the card is locked as declined; answering it later gives `409`.
With Auto mode on (`GET`/`PUT /v1/settings`, `auto_mode`) there is no card: the
agent is created at once and an `agent_created` event (`{name, id, by, auto: true}`,
`by` the requester's name) is recorded where the card would have gone.

**Relation to A2UI.** We checked Google's open
[A2UI](https://github.com/a2ui-project/a2ui) (Apache-2.0) as a base: the
current spec is **v1.0** ([spec](https://a2ui.org/specification/v1.0-a2ui/),
[repo copy](https://github.com/a2ui-project/a2ui/blob/main/specification/v1_0/docs/a2ui_protocol.md),
"Current", last updated 2026-06-08; basic catalog
`https://a2ui.org/specification/v1_0/catalogs/basic/catalog.json`). It is a
general streaming surface protocol (component adjacency lists, a bound data
model, client functions, validation rules) whose wire format changed with
every release (v0.8 → v0.9 → v0.9.1 → v1.0 within a year). For "ask the
operator a few questions" that is far more than a model should have to
author in one tool call or a renderer has to implement, so the card uses this
small own schema instead, designed to map 1:1 onto A2UI v1.0 later:

| ferrobot | A2UI v1.0 (basic catalog) |
| --- | --- |
| `QuestionCard` | `createSurface` with a `Card` → `Column` root; `title` → `Text` |
| `single` question | `ChoicePicker`, `variant: "mutuallyExclusive"`, `options: [{label, value}]` |
| `multi` question | `ChoicePicker`, `variant: "multipleSelection"` |
| `text` question / `allow_custom` | `TextField` (`variant: "shortText"`) |
| send button | `Button` (`variant: "primary"`) with an `action` event |
| `QuestionAnswer` (`values`, `custom`) | the action `context` (selected values, text) |
| `question_answered` | the renderer → agent `action` message |

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
