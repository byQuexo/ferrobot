//! Model providers. API keys go into the daemon's secret store and are never
//! returned, logged or handed to a sandbox.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use ferrobot_core::{Provider, ProviderKind};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::ApiResult;
use super::agents::explicit;
use crate::claude_code::{ClaudeCodeSettings, ClaudeCodeStatus, InstalledPlugin};
use crate::codex::{self, DeviceCode, LoginStatus};
use crate::error::{ApiError, ErrorBody};
use crate::models::{self, ModelList};
use crate::proxy::secret_key;
use crate::{AppState, lifecycle};

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ProviderView {
    #[serde(flatten)]
    provider: Provider,
    /// Whether an API key is stored. The key itself is never returned.
    has_key: bool,
}

async fn view(state: &AppState, provider: Provider) -> Result<ProviderView, ApiError> {
    let has_key = state
        .settings
        .secrets
        .get(&secret_key(provider.id))
        .await?
        .is_some();
    Ok(ProviderView { provider, has_key })
}

fn parse_pid(raw: &str) -> Result<Uuid, ApiError> {
    raw.parse()
        .map_err(|_| ApiError::NotFound(format!("provider {raw} not found")))
}

pub(crate) fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

pub(crate) fn check_key(key: &str) -> Result<(), ApiError> {
    if key.is_empty() || key.len() > 4096 || key.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(
            "api_key must be 1-4096 printable characters".into(),
        ));
    }
    Ok(())
}

/// Configured model providers, by name.
#[utoipa::path(get, path = "/v1/providers", tag = "providers",
    responses((status = 200, body = Vec<ProviderView>)))]
pub(crate) async fn list_providers(State(state): State<AppState>) -> ApiResult<Vec<ProviderView>> {
    let mut out = Vec::new();
    for p in state.store.list_providers().await? {
        out.push(view(&state, p).await?);
    }
    Ok(Json(out))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CreateProvider {
    /// Unique display name (1-64 bytes).
    #[schema(example = "openai-work")]
    name: String,
    kind: ProviderKind,
    /// Upstream override; required for `openai_compatible`.
    #[serde(default)]
    base_url: Option<String>,
    /// Required for `bedrock` and `vertex`.
    #[serde(default)]
    region: Option<String>,
    /// Google Cloud project; required for `vertex`.
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    default_model: Option<String>,
    /// Kept in the daemon's secret store; never returned.
    #[serde(default)]
    api_key: Option<String>,
}

/// Adds a model provider.
#[utoipa::path(post, path = "/v1/providers", tag = "providers",
    request_body = CreateProvider,
    responses((status = 201, body = ProviderView),
        (status = 400, body = ErrorBody),
        (status = 409, description = "Name already taken", body = ErrorBody)))]
pub(crate) async fn create_provider(
    State(state): State<AppState>,
    Json(req): Json<CreateProvider>,
) -> Result<(StatusCode, Json<ProviderView>), ApiError> {
    let provider = Provider {
        id: Uuid::new_v4(),
        name: req.name.trim().to_owned(),
        kind: req.kind,
        base_url: clean(req.base_url),
        region: clean(req.region),
        project: clean(req.project),
        default_model: clean(req.default_model),
        created_at: time::OffsetDateTime::now_utc(),
    };
    provider.validate().map_err(ApiError::BadRequest)?;
    let key = clean(req.api_key);
    if let Some(key) = &key {
        check_key(key)?;
    }
    let provider = state.store.create_provider(provider).await?;
    if let Some(key) = key {
        state
            .settings
            .secrets
            .set(&secret_key(provider.id), &key)
            .await?;
    }
    Ok((StatusCode::CREATED, Json(view(&state, provider).await?)))
}

#[derive(Debug, Deserialize, ToSchema)]
#[allow(
    clippy::option_option,
    reason = "omitted vs. explicit null in a PATCH body"
)]
pub(crate) struct UpdateProvider {
    name: Option<String>,
    /// `null` goes back to the kind's default upstream.
    #[serde(default, deserialize_with = "explicit")]
    #[schema(value_type = Option<String>)]
    base_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit")]
    #[schema(value_type = Option<String>)]
    region: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit")]
    #[schema(value_type = Option<String>)]
    project: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit")]
    #[schema(value_type = Option<String>)]
    default_model: Option<Option<String>>,
    /// New API key; `null` removes the stored one.
    #[serde(default, deserialize_with = "explicit")]
    #[schema(value_type = Option<String>)]
    api_key: Option<Option<String>>,
}

/// Edits a provider. Agents using it restart their computers (disks kept)
/// when region, project or default model change.
#[utoipa::path(patch, path = "/v1/providers/{id}", tag = "providers",
    params(("id" = String, Path, description = "Provider id (UUID)")),
    request_body = UpdateProvider,
    responses((status = 200, body = ProviderView),
        (status = 400, body = ErrorBody),
        (status = 404, description = "Unknown provider", body = ErrorBody),
        (status = 409, description = "Name already taken", body = ErrorBody)))]
pub(crate) async fn update_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateProvider>,
) -> ApiResult<ProviderView> {
    let id = parse_pid(&id)?;
    let old = state.store.get_provider(id).await?;
    let mut new = old.clone();
    if let Some(name) = req.name {
        name.trim().clone_into(&mut new.name);
    }
    let set = |field: &mut Option<String>, v: Option<Option<String>>| {
        if let Some(v) = v {
            *field = clean(v);
        }
    };
    set(&mut new.base_url, req.base_url);
    set(&mut new.region, req.region);
    set(&mut new.project, req.project);
    set(&mut new.default_model, req.default_model);
    new.validate().map_err(ApiError::BadRequest)?;
    let key = req.api_key.map(clean);
    if let Some(Some(key)) = &key {
        check_key(key)?;
    }
    state.store.update_provider(new.clone()).await?;
    match key {
        Some(Some(key)) => state.settings.secrets.set(&secret_key(id), &key).await?,
        Some(None) => state.settings.secrets.delete(&secret_key(id)).await?,
        None => {}
    }
    models::forget(&state, id);
    let baked_into_env = (&old.region, &old.project, &old.default_model)
        != (&new.region, &new.project, &new.default_model);
    if baked_into_env {
        for agent in state.store.list_agents().await? {
            if agent.provider_id == Some(id) {
                lifecycle::recreate(&state, agent.id).await?;
            }
        }
    }
    crate::credentials::clear_provider(&state, id).await;
    Ok(Json(view(&state, new).await?))
}

/// Deletes a provider and its stored key. Fails while agents use it.
#[utoipa::path(delete, path = "/v1/providers/{id}", tag = "providers",
    params(("id" = String, Path, description = "Provider id (UUID)")),
    responses((status = 204, description = "Deleted"),
        (status = 404, description = "Unknown provider", body = ErrorBody),
        (status = 409, description = "Agents still use it", body = ErrorBody)))]
pub(crate) async fn delete_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_pid(&id)?;
    state.store.delete_provider(id).await?;
    models::forget(&state, id);
    state.settings.secrets.delete(&secret_key(id)).await?;
    codex::forget(&state, id)
        .await
        .map_err(ApiError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn codex_provider(state: &AppState, raw: &str) -> Result<Uuid, ApiError> {
    let id = parse_pid(raw)?;
    if state.store.get_provider(id).await?.kind != ProviderKind::Codex {
        return Err(ApiError::Conflict(
            "ChatGPT sign-in is only for codex providers".into(),
        ));
    }
    Ok(id)
}

/// Starts a `ChatGPT` sign-in for a `codex` provider on the daemon host: open
/// `verification_url`, enter `user_code`, poll the status. Agents never see the tokens.
#[utoipa::path(post, path = "/v1/providers/{id}/chatgpt-login", tag = "providers",
    params(("id" = String, Path, description = "Provider id (UUID)")),
    responses((status = 200, body = DeviceCode),
        (status = 404, description = "Unknown provider", body = ErrorBody),
        (status = 409, description = "Not a codex provider", body = ErrorBody),
        (status = 502, description = "codex failed on the daemon host", body = ErrorBody)))]
pub(crate) async fn start_chatgpt_login(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<DeviceCode> {
    let id = codex_provider(&state, &id).await?;
    let code = codex::start_login(&state, id)
        .await
        .map_err(ApiError::Upstream)?;
    Ok(Json(code))
}

/// Whether a `codex` provider is signed in with `ChatGPT`.
#[utoipa::path(get, path = "/v1/providers/{id}/chatgpt-login", tag = "providers",
    params(("id" = String, Path, description = "Provider id (UUID)")),
    responses((status = 200, body = LoginStatus),
        (status = 404, description = "Unknown provider", body = ErrorBody),
        (status = 409, description = "Not a codex provider", body = ErrorBody)))]
pub(crate) async fn chatgpt_login_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<LoginStatus> {
    let id = codex_provider(&state, &id).await?;
    Ok(Json(codex::login_status(&state, id).await))
}

/// Whether the daemon host's own `claude` CLI is installed and signed in, for Claude Code
/// providers. Only the login state is returned, never the account.
#[utoipa::path(get, path = "/v1/claude-code", tag = "providers",
    responses((status = 200, body = ClaudeCodeStatus)))]
pub(crate) async fn claude_code_status(
    State(state): State<AppState>,
) -> ApiResult<ClaudeCodeStatus> {
    Ok(Json(crate::claude_code::status(&state).await))
}

/// Hooks and plugins for agents on Claude Code. Only the operator sets them; agents cannot.
#[utoipa::path(get, path = "/v1/claude-code/settings", tag = "providers",
    responses((status = 200, body = ClaudeCodeSettings)))]
pub(crate) async fn claude_code_settings(
    State(state): State<AppState>,
) -> ApiResult<ClaudeCodeSettings> {
    crate::claude_code::load_settings(&state)
        .await
        .map(Json)
        .map_err(ApiError::Internal)
}

/// Replaces the hooks and plugins for agents on Claude Code, from their next turn on. Hooks
/// run on this computer, outside the sandboxes; the user's own Claude Code settings stay out.
#[utoipa::path(put, path = "/v1/claude-code/settings", tag = "providers",
    request_body = ClaudeCodeSettings,
    responses((status = 200, body = ClaudeCodeSettings),
        (status = 400, description = "Invalid hook or plugin id", body = ErrorBody)))]
pub(crate) async fn put_claude_code_settings(
    State(state): State<AppState>,
    Json(mut settings): Json<ClaudeCodeSettings>,
) -> ApiResult<ClaudeCodeSettings> {
    crate::claude_code::validate(&mut settings).map_err(ApiError::BadRequest)?;
    crate::claude_code::save_settings(&state, &settings).await?;
    Ok(Json(settings))
}

/// Plugins installed in the host's Claude Code (`claude plugin list`).
#[utoipa::path(get, path = "/v1/claude-code/plugins", tag = "providers",
    responses((status = 200, body = [InstalledPlugin]),
        (status = 502, description = "Claude Code did not answer", body = ErrorBody)))]
pub(crate) async fn claude_code_plugins(
    State(state): State<AppState>,
) -> ApiResult<Vec<InstalledPlugin>> {
    crate::claude_code::installed_plugins(&state)
        .await
        .map(Json)
        .map_err(ApiError::Upstream)
}

/// Models to offer for a provider: its own list fetched with the stored key (cached for a
/// few minutes), else a short built-in list.
#[utoipa::path(get, path = "/v1/providers/{id}/models", tag = "providers",
    params(("id" = String, Path, description = "Provider id (UUID)")),
    responses((status = 200, body = ModelList),
        (status = 404, description = "Unknown provider", body = ErrorBody)))]
pub(crate) async fn provider_models(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<ModelList> {
    let provider = state.store.get_provider(parse_pid(&id)?).await?;
    Ok(Json(models::list(&state, &provider).await))
}

/// The built-in model list for a provider kind, e.g. before the provider exists.
#[utoipa::path(get, path = "/v1/provider-kinds/{kind}/models", tag = "providers",
    params(("kind" = ProviderKind, Path, description = "Provider kind")),
    responses((status = 200, body = ModelList),
        (status = 404, description = "Unknown kind", body = ErrorBody)))]
pub(crate) async fn kind_models(Path(kind): Path<String>) -> ApiResult<ModelList> {
    let kind = kind
        .parse()
        .map_err(|_| ApiError::NotFound(format!("provider kind {kind} not found")))?;
    Ok(Json(models::suggested(kind)))
}
