//! `/v1/logins`: logins for CLIs in the sandboxes, set up by the operator. Tokens go to the secret
//! store and are never returned; agents get stand-ins and the login proxy (`egress.rs`).

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use ferrobot_core::AgentId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ApiResult;
use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::logins::{self, Login};

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct LoginView {
    #[serde(flatten)]
    login: Login,
    /// Whether a token is stored. The token itself is never returned.
    has_token: bool,
}

async fn view(state: &AppState, login: Login) -> LoginView {
    LoginView {
        has_token: logins::token(state, &login.id).await.is_some(),
        login,
    }
}

/// Every login for CLIs, with its agents.
#[utoipa::path(get, path = "/v1/logins", tag = "clis",
    responses((status = 200, body = [LoginView])))]
pub(crate) async fn get_logins(State(state): State<AppState>) -> ApiResult<Vec<LoginView>> {
    let mut out = Vec::new();
    for login in logins::all(&state).await {
        out.push(view(&state, login).await);
    }
    Ok(Json(out))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct LoginSpec {
    /// API hosts whose requests get the real token, e.g. `api.example.com`.
    hosts: Vec<String>,
    /// Environment variables the CLIs read their token from, e.g. `EXAMPLE_API_KEY`.
    env: Vec<String>,
    /// Program names that use the login, e.g. `example`.
    #[serde(default)]
    programs: Vec<String>,
    /// Ask before every command that uses the login, unless Auto mode is on.
    #[serde(default)]
    confirm: bool,
    /// The agents whose CLIs use the login.
    #[serde(default)]
    #[schema(value_type = Vec<String>)]
    agents: Vec<AgentId>,
}

fn words(list: Vec<String>, lower: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in list {
        let w = w.trim();
        let w = if lower {
            w.to_ascii_lowercase()
        } else {
            w.to_owned()
        };
        if !w.is_empty() && !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// Restarts the computers of `agents` (files kept), so the proxy, stand-ins and trust reach or
/// leave them.
fn restart(state: &AppState, agents: impl IntoIterator<Item = AgentId>) {
    for agent in agents {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::lifecycle::recreate(&state, agent).await {
                tracing::warn!(%agent, error = %e, "computer not restarted for a login change");
            }
        });
    }
}

/// Adds or replaces a login. Agents it reaches or leaves restart their computers (files kept);
/// so do all its agents when its hosts or variables change.
#[utoipa::path(put, path = "/v1/logins/{id}", tag = "clis",
    params(("id" = String, Path, description = "The login's id, e.g. `github`")),
    request_body = LoginSpec,
    responses((status = 200, body = LoginView),
        (status = 400, description = "An invalid field or an unknown agent", body = ErrorBody)))]
pub(crate) async fn put_login(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<LoginSpec>,
) -> ApiResult<LoginView> {
    let known: HashSet<AgentId> = state
        .store
        .list_agents()
        .await?
        .into_iter()
        .map(|a| a.id)
        .collect();
    if req.agents.iter().any(|a| !known.contains(a)) {
        return Err(ApiError::BadRequest("an unknown agent".into()));
    }
    let mut agents = req.agents;
    agents.dedup();
    let login = Login {
        id,
        hosts: words(req.hosts, true),
        env: words(req.env, false),
        programs: words(req.programs, false),
        confirm: req.confirm,
        agents,
    };
    let mut all = logins::all(&state).await;
    login.validate(&all).map_err(ApiError::BadRequest)?;
    let old = all
        .iter()
        .position(|l| l.id == login.id)
        .map(|i| all.remove(i));
    let restarted: HashSet<AgentId> = match &old {
        Some(old) if old.hosts == login.hosts && old.env == login.env => {
            let before: HashSet<_> = old.agents.iter().copied().collect();
            let after: HashSet<_> = login.agents.iter().copied().collect();
            before.symmetric_difference(&after).copied().collect()
        }
        Some(old) => old.agents.iter().chain(&login.agents).copied().collect(),
        None => login.agents.iter().copied().collect(),
    };
    all.push(login.clone());
    logins::save(&state, &all).await?;
    restart(&state, restarted);
    Ok(Json(view(&state, login).await))
}

/// Removes a login, its token and its CA; its agents restart their computers (files kept).
#[utoipa::path(delete, path = "/v1/logins/{id}", tag = "clis",
    params(("id" = String, Path, description = "The login's id")),
    responses((status = 204, description = "Removed"),
        (status = 404, description = "No such login", body = ErrorBody)))]
pub(crate) async fn delete_login(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let mut all = logins::all(&state).await;
    let Some(i) = all.iter().position(|l| l.id == id) else {
        return Err(ApiError::NotFound(format!("login {id:?} not found")));
    };
    let gone = all.remove(i);
    logins::save(&state, &all).await?;
    state.settings.secrets.delete(&gone.secret_key()).await?;
    crate::egress::forget(&state, &gone.id)
        .await
        .map_err(ApiError::Internal)?;
    restart(&state, gone.agents);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct LoginToken {
    /// The service's token, e.g. a fine-grained GitHub token; `null` removes it.
    token: Option<String>,
}

/// Stores or removes the login's token. Kept in the daemon's secret store, never returned.
#[utoipa::path(put, path = "/v1/logins/{id}/token", tag = "clis",
    params(("id" = String, Path, description = "The login's id")),
    request_body = LoginToken,
    responses((status = 200, body = LoginView),
        (status = 400, description = "Not a token", body = ErrorBody),
        (status = 404, description = "No such login", body = ErrorBody)))]
pub(crate) async fn put_login_token(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<LoginToken>,
) -> ApiResult<LoginView> {
    let Some(login) = logins::all(&state).await.into_iter().find(|l| l.id == id) else {
        return Err(ApiError::NotFound(format!("login {id:?} not found")));
    };
    let secrets = &state.settings.secrets;
    match req.token.as_deref().map(str::trim) {
        Some(token) if !token.is_empty() => {
            if token.len() > 1024 || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(ApiError::BadRequest(
                    "a token is one word of up to 1024 characters".into(),
                ));
            }
            secrets.set(&login.secret_key(), token).await?;
        }
        _ => secrets.delete(&login.secret_key()).await?,
    }
    Ok(Json(view(&state, login).await))
}
