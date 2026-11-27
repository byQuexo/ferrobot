//! Incoming and outgoing webhooks (`/v1/webhooks`). Incoming requests arrive on `/hooks/...`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use ferrobot_core::AgentId;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use super::ApiResult;
use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::otel::ExportStatus;
use crate::webhooks::{
    self, IncomingWebhook, OutgoingWebhook, StoredIncoming, WebhookEvent, WebhookFormat, hook_url,
    url_secret,
};

const MAX_NAME_CHARS: usize = 64;
const MAX_URL_BYTES: usize = 2048;

/// The webhooks. Neither secrets nor outgoing URLs are ever returned.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct WebhooksView {
    incoming: Vec<IncomingWebhook>,
    outgoing: Vec<OutgoingWebhookView>,
}

/// An outgoing webhook and how its last delivery went.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct OutgoingWebhookView {
    #[serde(flatten)]
    hook: OutgoingWebhook,
    /// `null`: nothing sent since the daemon started.
    last_delivery: Option<ExportStatus>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct NewIncomingWebhook {
    /// 1-64 characters; agents see it as the sender, `webhook:<name>`.
    name: String,
    agent_id: AgentId,
}

/// An incoming webhook with its URL. The URL holds the secret and is shown only this once.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct IncomingWebhookCreated {
    #[serde(flatten)]
    hook: IncomingWebhook,
    /// `<daemon>/hooks/{id}/{secret}`; from the internet, through a tunnel to `/hooks/` only.
    url: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct NewOutgoingWebhook {
    /// 1-64 characters.
    name: String,
    /// An http or https URL, e.g. a Discord or Slack webhook; kept in the secret store.
    url: String,
    /// At least one.
    events: Vec<WebhookEvent>,
    /// Only these agents' events; empty or omitted: every agent's.
    #[serde(default)]
    agents: Vec<AgentId>,
    format: WebhookFormat,
}

fn hook_name(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    if !super::is_printable(name, MAX_NAME_CHARS) {
        return Err(ApiError::BadRequest(format!(
            "name must be 1-{MAX_NAME_CHARS} printable characters"
        )));
    }
    Ok(name.to_owned())
}

fn no_hook(id: i64) -> ApiError {
    ApiError::NotFound(format!("webhook {id} not found"))
}

/// Incoming and outgoing webhooks, oldest first.
#[utoipa::path(get, path = "/v1/webhooks", tag = "webhooks",
    responses((status = 200, body = WebhooksView)))]
pub(crate) async fn list_webhooks(State(state): State<AppState>) -> ApiResult<WebhooksView> {
    let stored = webhooks::load(&state).await?;
    Ok(Json(WebhooksView {
        incoming: stored.incoming.into_iter().map(|h| h.hook).collect(),
        outgoing: stored
            .outgoing
            .into_iter()
            .map(|hook| OutgoingWebhookView {
                last_delivery: state.webhooks.last_delivery(hook.id),
                hook,
            })
            .collect(),
    }))
}

/// Creates an incoming webhook for an agent. Its URL is in this response only.
#[utoipa::path(post, path = "/v1/webhooks/incoming", tag = "webhooks",
    request_body = NewIncomingWebhook,
    responses((status = 201, body = IncomingWebhookCreated),
        (status = 400, description = "Bad name", body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn create_incoming(
    State(state): State<AppState>,
    Json(req): Json<NewIncomingWebhook>,
) -> Result<(StatusCode, Json<IncomingWebhookCreated>), ApiError> {
    let name = hook_name(&req.name)?;
    state.store.get_agent(req.agent_id).await?;
    let secret = crate::lifecycle::new_token();
    let _edit = state.webhooks.edit.lock().await;
    let mut stored = webhooks::load(&state).await?;
    let hook = IncomingWebhook {
        id: stored.new_id(),
        name,
        agent_id: req.agent_id,
        created_at: OffsetDateTime::now_utc(),
    };
    stored.incoming.push(StoredIncoming {
        hook: hook.clone(),
        secret_hash: ferrobot_store::hash_token(&secret),
    });
    webhooks::save(&state, &stored).await?;
    let url = hook_url(&state, hook.id, &secret);
    Ok((
        StatusCode::CREATED,
        Json(IncomingWebhookCreated { hook, url }),
    ))
}

/// Gives an incoming webhook a new secret; the old URL stops working at once.
#[utoipa::path(post, path = "/v1/webhooks/incoming/{id}/secret", tag = "webhooks",
    params(("id" = i64, Path, description = "Webhook id")),
    responses((status = 200, body = IncomingWebhookCreated),
        (status = 404, description = "Unknown webhook", body = ErrorBody)))]
pub(crate) async fn regenerate_incoming(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<IncomingWebhookCreated> {
    let secret = crate::lifecycle::new_token();
    let _edit = state.webhooks.edit.lock().await;
    let mut stored = webhooks::load(&state).await?;
    let entry = stored
        .incoming
        .iter_mut()
        .find(|h| h.hook.id == id)
        .ok_or_else(|| no_hook(id))?;
    entry.secret_hash = ferrobot_store::hash_token(&secret);
    let hook = entry.hook.clone();
    webhooks::save(&state, &stored).await?;
    let url = hook_url(&state, id, &secret);
    Ok(Json(IncomingWebhookCreated { hook, url }))
}

/// Deletes an incoming webhook; its URL stops working.
#[utoipa::path(delete, path = "/v1/webhooks/incoming/{id}", tag = "webhooks",
    params(("id" = i64, Path, description = "Webhook id")),
    responses((status = 204, description = "Deleted"),
        (status = 404, description = "Unknown webhook", body = ErrorBody)))]
pub(crate) async fn delete_incoming(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let _edit = state.webhooks.edit.lock().await;
    let mut stored = webhooks::load(&state).await?;
    let before = stored.incoming.len();
    stored.incoming.retain(|h| h.hook.id != id);
    if stored.incoming.len() == before {
        return Err(no_hook(id));
    }
    webhooks::save(&state, &stored).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Adds an outgoing webhook; the events it gets are posted to its URL in the background.
#[utoipa::path(post, path = "/v1/webhooks/outgoing", tag = "webhooks",
    request_body = NewOutgoingWebhook,
    responses((status = 201, body = OutgoingWebhookView),
        (status = 400, description = "Bad name, URL or events", body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody)))]
pub(crate) async fn create_outgoing(
    State(state): State<AppState>,
    Json(req): Json<NewOutgoingWebhook>,
) -> Result<(StatusCode, Json<OutgoingWebhookView>), ApiError> {
    let name = hook_name(&req.name)?;
    let url = req.url.trim();
    let scheme_ok = reqwest::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
    if !scheme_ok || url.len() > MAX_URL_BYTES {
        return Err(ApiError::BadRequest(
            "the URL must be an http or https URL".into(),
        ));
    }
    let mut events = req.events;
    events.dedup();
    if events.is_empty() {
        return Err(ApiError::BadRequest("pick at least one event".into()));
    }
    let agents = super::known_agents(&state, req.agents).await?;
    let _edit = state.webhooks.edit.lock().await;
    let mut stored = webhooks::load(&state).await?;
    let hook = OutgoingWebhook {
        id: stored.new_id(),
        name,
        events,
        agents,
        format: req.format,
        created_at: OffsetDateTime::now_utc(),
    };
    state
        .settings
        .secrets
        .set(&url_secret(hook.id), url)
        .await?;
    stored.outgoing.push(hook.clone());
    webhooks::save(&state, &stored).await?;
    Ok((
        StatusCode::CREATED,
        Json(OutgoingWebhookView {
            hook,
            last_delivery: None,
        }),
    ))
}

/// Deletes an outgoing webhook and its stored URL.
#[utoipa::path(delete, path = "/v1/webhooks/outgoing/{id}", tag = "webhooks",
    params(("id" = i64, Path, description = "Webhook id")),
    responses((status = 204, description = "Deleted"),
        (status = 404, description = "Unknown webhook", body = ErrorBody)))]
pub(crate) async fn delete_outgoing(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let _edit = state.webhooks.edit.lock().await;
    let mut stored = webhooks::load(&state).await?;
    let before = stored.outgoing.len();
    stored.outgoing.retain(|h| h.id != id);
    if stored.outgoing.len() == before {
        return Err(no_hook(id));
    }
    webhooks::save(&state, &stored).await?;
    state.settings.secrets.delete(&url_secret(id)).await?;
    state.webhooks.forget(id);
    Ok(StatusCode::NO_CONTENT)
}
