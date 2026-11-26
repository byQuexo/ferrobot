//! `/v1/logins`: logins for CLIs in the sandboxes (GitHub first). Tokens go to the secret store
//! and are never returned; agents get stand-ins and the login proxy (`egress.rs`).

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use ferrobot_core::AgentId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ApiResult;
use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::logins::{self, LoginKind};

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct LoginView {
    kind: LoginKind,
    /// Whether a token is stored. The token itself is never returned.
    has_token: bool,
    /// The agents whose CLIs use this login.
    #[schema(value_type = Vec<String>)]
    agents: Vec<AgentId>,
    /// The hosts whose requests get the token.
    hosts: Vec<&'static str>,
}

async fn view(state: &AppState, kind: LoginKind) -> LoginView {
    LoginView {
        kind,
        has_token: logins::token(state, kind).await.is_some(),
        agents: logins::assignments(state)
            .await
            .remove(&kind)
            .unwrap_or_default(),
        hosts: kind.hosts().to_vec(),
    }
}

/// Every login for CLIs, with its agents.
#[utoipa::path(get, path = "/v1/logins", tag = "clis",
    responses((status = 200, body = [LoginView])))]
pub(crate) async fn get_logins(State(state): State<AppState>) -> ApiResult<Vec<LoginView>> {
    let mut out = Vec::new();
    for kind in LoginKind::ALL {
        out.push(view(&state, kind).await);
    }
    Ok(Json(out))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct LoginAgents {
    #[schema(value_type = Vec<String>)]
    agents: Vec<AgentId>,
}

/// Lends the login to exactly these agents. Their computers restart (files kept) so the proxy
/// and stand-in token reach them, or leave them.
#[utoipa::path(put, path = "/v1/logins/{kind}", tag = "clis",
    params(("kind" = LoginKind, Path, description = "The login")),
    request_body = LoginAgents,
    responses((status = 200, body = LoginView),
        (status = 400, description = "An unknown agent", body = ErrorBody)))]
pub(crate) async fn put_login_agents(
    State(state): State<AppState>,
    Path(kind): Path<LoginKind>,
    Json(req): Json<LoginAgents>,
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
    let mut assigned = logins::assignments(&state).await;
    let before: HashSet<AgentId> = assigned.get(&kind).into_iter().flatten().copied().collect();
    let mut agents = req.agents;
    agents.dedup();
    let after: HashSet<AgentId> = agents.iter().copied().collect();
    assigned.insert(kind, agents);
    logins::save(&state, &assigned).await?;
    for agent in before.symmetric_difference(&after).copied() {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::lifecycle::recreate(&state, agent).await {
                tracing::warn!(%agent, error = %e, "computer not restarted for a login change");
            }
        });
    }
    Ok(Json(view(&state, kind).await))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct LoginToken {
    /// The service's token, e.g. a fine-grained GitHub token; `null` removes it.
    token: Option<String>,
}

/// Stores or removes the login's token. Kept in the daemon's secret store, never returned.
#[utoipa::path(put, path = "/v1/logins/{kind}/token", tag = "clis",
    params(("kind" = LoginKind, Path, description = "The login")),
    request_body = LoginToken,
    responses((status = 200, body = LoginView),
        (status = 400, description = "Not a token", body = ErrorBody)))]
pub(crate) async fn put_login_token(
    State(state): State<AppState>,
    Path(kind): Path<LoginKind>,
    Json(req): Json<LoginToken>,
) -> ApiResult<LoginView> {
    let secrets = &state.settings.secrets;
    match req.token.as_deref().map(str::trim) {
        Some(token) if !token.is_empty() => {
            if token.len() > 1024 || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(ApiError::BadRequest(
                    "a token is one word of up to 1024 characters".into(),
                ));
            }
            secrets.set(kind.secret_key(), token).await?;
        }
        _ => secrets.delete(kind.secret_key()).await?,
    }
    Ok(Json(view(&state, kind).await))
}
