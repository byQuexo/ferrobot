//! API contracts generated from the Rust types; committed under `protocol/` and kept in sync
//! by `tests/specs.rs` (regenerate with `FERROBOT_UPDATE_SPECS=1`).

use ferrobot_core::protocol::{DaemonFrame, RunnerFrame};
use ferrobot_core::question::{QuestionAnswered, QuestionEvent};
use serde_json::{Map, Value, json};
use utoipa::OpenApi;
use utoipa::openapi::OpenApi as Spec;

use crate::UiEvent;
use crate::api;

/// File name of the `OpenAPI` document under `protocol/`.
pub const OPENAPI_FILE: &str = "openapi.json";
/// File name of the UI WebSocket event schema under `protocol/`.
pub const UI_EVENTS_FILE: &str = "ui-events.schema.json";
/// File name of the runner wire protocol schema under `protocol/`.
pub const RUNNER_PROTOCOL_FILE: &str = "runner-protocol.schema.json";
/// File name of the question card schema (version 1) under `protocol/`.
pub const QUESTION_CARD_FILE: &str = "question-card.v1.schema.json";

#[derive(OpenApi)]
#[openapi(
    info(
        title = "ferrobot daemon API",
        description = "Operator API of `ferrobotd` (version 1, all paths under `/v1`). \
            Served on the loopback UI listener (default `127.0.0.1:7878`). Every \
            endpoint except `GET /v1/health` needs the operator token as \
            `Authorization: Bearer <token>`; `GET` requests (the WebSocket, avatar \
            images) may pass `?access_token=<token>` instead. The daemon generates \
            the token on first start in `<data dir>/api-token` (mode 0600) unless \
            `--api-token` / `FERROBOT_API_TOKEN` sets one. Live updates: `GET /v1/ws` streams `UiEvent` frames, \
            see `ui-events.schema.json`. The separate agent listener (runner \
            WebSocket and MCP, bearer-token auth) is not part of this document; \
            its frames are described in `runner-protocol.schema.json`.",
        license(name = "MIT OR Apache-2.0", identifier = "MIT OR Apache-2.0"),
    ),
    servers((url = "http://127.0.0.1:7878", description = "Default local daemon")),
    modifiers(&SecurityAddon),
    security(("bearer" = []), ("access_token" = [])),
    paths(
        api::health,
        api::agents::list_agents,
        api::agents::create_agent,
        api::agents::get_agent,
        api::agents::update_agent,
        api::agents::delete_agent,
        api::agents::get_avatar,
        api::agents::put_avatar,
        api::agents::delete_avatar,
        api::chat::list_messages,
        api::chat::post_message,
        api::chat::list_events,
        api::chat::mark_read,
        api::chat::list_conversations,
        api::chat::conversation_thread,
        api::chat::interrupt,
        api::chat::retry,
        api::chat::answer_question,
        api::memory::list_memory,
        api::memory::put_memory,
        api::memory::delete_memory,
        api::memory::list_global_memory,
        api::memory::put_global_memory,
        api::memory::delete_global_memory,
        api::routines::list_routines,
        api::routines::create_routine,
        api::routines::update_routine,
        api::routines::delete_routine,
        api::routines::run_routine,
        api::routines::list_runs,
        api::sandbox::sandbox_info,
        api::sandbox::stop,
        api::sandbox::wake,
        api::sandbox::destroy_sandbox,
        api::sandbox::exec,
        api::sandbox::list_snapshots,
        api::sandbox::create_snapshot,
        api::sandbox::restore_snapshot,
        api::sandbox::delete_snapshot,
        api::providers::list_providers,
        api::providers::create_provider,
        api::providers::update_provider,
        api::providers::delete_provider,
        api::providers::start_chatgpt_login,
        api::providers::chatgpt_login_status,
        api::providers::claude_code_status,
        api::providers::provider_models,
        api::providers::kind_models,
        api::connectors::list_connectors,
        api::connectors::create_connector,
        api::connectors::update_connector,
        api::connectors::delete_connector,
        api::connectors::agent_connectors,
        api::connectors::set_agent_connectors,
        api::connectors::start_oauth,
        api::connectors::sign_out,
        api::connectors::check_connector,
        api::connectors::oauth_callback,
        api::clis::get_clis,
        api::clis::put_clis,
        api::voice::get_voice,
        api::voice::put_voice,
        api::voice::put_voice_key,
        api::voice::speech,
        api::voice::speech_link,
        api::voice::catalog,
        api::voice::live,
        api::voice::transcription,
        api::voice::get_call,
        api::voice::put_call,
        api::voice::post_call_message,
        api::voice::list_rooms,
        api::voice::get_room,
        api::settings::get_settings,
        api::settings::put_settings,
        api::usage::get_usage,
        api::usage::get_prices,
        api::usage::put_prices,
        api::update::get_update,
        api::update::start_update,
        api::diagnostics::get_diagnostics,
        api::diagnostics::get_log,
        api::files::shared_file,
        api::files::page,
        api::ws::ui_ws,
    ),
    components(schemas(
        UiEvent,
        QuestionEvent,
        QuestionAnswered,
        ferrobot_sandbox::SleepLevel,
        crate::oauth::SignIn
    )),
    tags(
        (name = "system", description = "Daemon status"),
        (name = "agents", description = "Agent registry and profiles"),
        (name = "chat", description = "Messages, turn events and the live stream"),
        (name = "memory", description = "Per-agent and global long-term memory"),
        (name = "routines", description = "Scheduled prompts"),
        (name = "sandbox", description = "Each agent's own computer: sleep, wake, snapshots"),
        (name = "providers", description = "Model providers and their API keys"),
        (name = "connectors", description = "Remote MCP servers whose tools agents can use"),
        (name = "clis", description = "Command-line tools in every agent's sandbox"),
        (name = "voice", description = "Speech and transcription for the operator's chat"),
        (name = "usage", description = "Model calls, tokens and estimated cost"),
        (name = "settings", description = "Daemon-wide operator settings"),
    )
)]
struct ApiDoc;

/// Declares the operator token schemes referenced by `security`.
struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut Spec) {
        use utoipa::openapi::security::{
            ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme,
        };
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "Operator token from `<data dir>/api-token` or `--api-token`.",
                    ))
                    .build(),
            ),
        );
        components.add_security_scheme(
            "access_token",
            SecurityScheme::ApiKey(ApiKey::Query(ApiKeyValue::with_description(
                crate::auth::QUERY_PARAM,
                "The same token as a query parameter; accepted on GET requests only \
                 (WebSocket upgrades, images).",
            ))),
        );
        // Every authenticated operation can answer 401.
        let unauthorized = utoipa::openapi::ResponseBuilder::new()
            .description("Missing or wrong API token")
            .content(
                "application/json",
                utoipa::openapi::ContentBuilder::new()
                    .schema(Some(utoipa::openapi::Ref::from_schema_name("ErrorBody")))
                    .build(),
            )
            .build();
        for (path, item) in &mut openapi.paths.paths {
            if path == "/v1/health" {
                continue;
            }
            for op in [
                &mut item.get,
                &mut item.post,
                &mut item.put,
                &mut item.patch,
                &mut item.delete,
            ]
            .into_iter()
            .flatten()
            {
                op.responses
                    .responses
                    .insert("401".into(), unauthorized.clone().into());
            }
        }
    }
}

#[derive(OpenApi)]
#[openapi(components(schemas(UiEvent)))]
struct UiEventsDoc;

#[derive(OpenApi)]
#[openapi(components(schemas(DaemonFrame, RunnerFrame)))]
struct RunnerDoc;

#[derive(OpenApi)]
#[openapi(components(schemas(QuestionEvent, QuestionAnswered)))]
struct QuestionDoc;

/// JSON Schema (2020-12) for question cards: the `ask_user` MCP tool input
/// (`QuestionCard`) and the payloads of `question` / `question_answered` events.
pub fn question_card_schema() -> Value {
    json_schema(
        &QuestionDoc::openapi(),
        QUESTION_CARD_FILE,
        "ferrobot question card (version 1)",
        "Payload of a `question` event (`QuestionEvent`, the card an agent showed with the \
         `ask_user` MCP tool) or of a `question_answered` event (`QuestionAnswered`).",
        &json!({ "oneOf": [
            { "$ref": "#/$defs/QuestionEvent" },
            { "$ref": "#/$defs/QuestionAnswered" }
        ] }),
    )
}

/// The `OpenAPI` 3.1 document for the UI API.
pub fn openapi() -> Spec {
    ApiDoc::openapi()
}

/// JSON Schema (2020-12) for frames on `GET /v1/ws`.
pub fn ui_events_schema() -> Value {
    json_schema(
        &UiEventsDoc::openapi(),
        UI_EVENTS_FILE,
        "ferrobot UI event",
        "One JSON text frame pushed by the daemon on `GET /v1/ws`.",
        &json!({ "$ref": "#/$defs/UiEvent" }),
    )
}

/// JSON Schema (2020-12) for the daemon <-> runner WebSocket frames.
pub fn runner_protocol_schema() -> Value {
    json_schema(
        &RunnerDoc::openapi(),
        RUNNER_PROTOCOL_FILE,
        "ferrobot runner protocol frame",
        "A frame on the runner WebSocket (`/runner/ws`, agent listener): \
         `DaemonFrame` from daemon to runner, `RunnerFrame` from runner to daemon.",
        &json!({ "oneOf": [
            { "$ref": "#/$defs/DaemonFrame" },
            { "$ref": "#/$defs/RunnerFrame" }
        ] }),
    )
}

/// Turns the component schemas of `doc` into a standalone JSON Schema with `$defs`.
fn json_schema(doc: &Spec, id: &str, title: &str, description: &str, root: &Value) -> Value {
    let components = doc
        .components
        .as_ref()
        .map(|c| serde_json::to_value(&c.schemas).unwrap_or_default())
        .unwrap_or_default();
    let mut out = Map::new();
    out.insert(
        "$schema".into(),
        "https://json-schema.org/draft/2020-12/schema".into(),
    );
    out.insert("$id".into(), id.into());
    out.insert("title".into(), title.into());
    out.insert("description".into(), description.into());
    if let Value::Object(root) = root {
        out.extend(root.clone());
    }
    out.insert("$defs".into(), rewrite_refs(components));
    Value::Object(out)
}

fn rewrite_refs(value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(s.replace("#/components/schemas/", "#/$defs/")),
        Value::Array(items) => Value::Array(items.into_iter().map(rewrite_refs).collect()),
        Value::Object(map) => {
            Value::Object(map.into_iter().map(|(k, v)| (k, rewrite_refs(v))).collect())
        }
        other => other,
    }
}

/// Pretty JSON with a trailing newline, as committed under `protocol/`.
pub fn to_pretty(value: &impl serde::Serialize) -> String {
    let mut s = serde_json::to_string_pretty(value).unwrap_or_default();
    s.push('\n');
    s
}

/// All generated files as `(file name, contents)`.
pub fn generated_files() -> Vec<(&'static str, String)> {
    vec![
        (OPENAPI_FILE, to_pretty(&openapi())),
        (UI_EVENTS_FILE, to_pretty(&ui_events_schema())),
        (RUNNER_PROTOCOL_FILE, to_pretty(&runner_protocol_schema())),
        (QUESTION_CARD_FILE, to_pretty(&question_card_schema())),
    ]
}
