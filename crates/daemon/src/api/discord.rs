//! The Discord bridge's settings (`/v1/discord`); the bot token goes to the secret store.

use std::collections::HashSet;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ApiResult;
use super::providers::set_secret;
use crate::AppState;
use crate::discord::{self, ChannelStatus, DiscordSettings, MAX_BRIDGES, MAX_USERS};
use crate::error::{ApiError, ErrorBody};

/// The bridge's settings, whether a bot token is stored, and how each channel is doing.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct DiscordView {
    #[serde(flatten)]
    settings: DiscordSettings,
    /// A bot token is in the secret store; it is never returned.
    has_token: bool,
    channels: Vec<ChannelStatus>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct DiscordToken {
    /// The bot token from Discord's developer portal; `null` removes it.
    token: Option<String>,
}

async fn view(state: &AppState, settings: DiscordSettings) -> ApiResult<DiscordView> {
    Ok(Json(DiscordView {
        has_token: state.settings.secrets.get(discord::SECRET).await?.is_some(),
        channels: state.discord.status(),
        settings,
    }))
}

/// The Discord bridge.
#[utoipa::path(get, path = "/v1/discord", tag = "settings",
    responses((status = 200, body = DiscordView)))]
pub(crate) async fn get_discord(State(state): State<AppState>) -> ApiResult<DiscordView> {
    let settings = discord::load(&state).await?;
    view(&state, settings).await
}

/// Replaces the bridge's settings: on or off, the allowed Discord users, and which channel
/// talks to which agent. Changes apply within seconds.
#[utoipa::path(put, path = "/v1/discord", tag = "settings",
    request_body = DiscordSettings,
    responses((status = 200, body = DiscordView),
        (status = 400, description = "A bad id, too many entries or an unknown agent", body = ErrorBody)))]
pub(crate) async fn put_discord(
    State(state): State<AppState>,
    Json(mut settings): Json<DiscordSettings>,
) -> ApiResult<DiscordView> {
    let trim = |ids: &mut Vec<String>| {
        for id in ids.iter_mut() {
            let t = id.trim().to_owned();
            *id = t;
        }
    };
    trim(&mut settings.users);
    let mut seen = HashSet::new();
    settings.users.retain(|u| seen.insert(u.clone()));
    for b in &mut settings.bridges {
        let t = b.channel_id.trim().to_owned();
        b.channel_id = t;
    }
    if settings.users.len() > MAX_USERS || settings.bridges.len() > MAX_BRIDGES {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_USERS} users and {MAX_BRIDGES} channels"
        )));
    }
    if let Some(bad) = settings
        .users
        .iter()
        .chain(settings.bridges.iter().map(|b| &b.channel_id))
        .find(|id| !discord::is_snowflake(id))
    {
        return Err(ApiError::BadRequest(format!(
            "{bad} is not a Discord id (turn on Developer Mode in Discord and use Copy ID)"
        )));
    }
    let mut channels = HashSet::new();
    if !settings
        .bridges
        .iter()
        .all(|b| channels.insert(b.channel_id.clone()))
    {
        return Err(ApiError::BadRequest("a channel talks to one agent".into()));
    }
    for b in &settings.bridges {
        state.store.get_agent(b.agent_id).await?;
    }
    discord::save(&state, &settings).await?;
    view(&state, settings).await
}

/// Stores or removes the bot token, kept in the daemon's secret store.
#[utoipa::path(put, path = "/v1/discord/token", tag = "settings",
    request_body = DiscordToken,
    responses((status = 200, body = DiscordView), (status = 400, body = ErrorBody)))]
pub(crate) async fn put_discord_token(
    State(state): State<AppState>,
    Json(req): Json<DiscordToken>,
) -> ApiResult<DiscordView> {
    set_secret(&state, discord::SECRET, req.token).await?;
    let settings = discord::load(&state).await?;
    view(&state, settings).await
}
