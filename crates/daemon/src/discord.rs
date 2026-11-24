//! The Discord bridge: in a bridged channel, messages from the Discord users the operator
//! allowed reach an agent as the operator's own, and the agent's reply goes back to the channel.
//! The daemon polls Discord's REST API with the operator's bot token (secret store
//! `discord:bot`), so it needs no public URL. The token never leaves the daemon.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use ferrobot_core::{AgentId, Sender};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::error::ApiError;
use crate::{AppState, UiEvent};

const SETTING: &str = "discord";
/// The secret store entry holding the bot token.
pub const SECRET: &str = "discord:bot";
pub const MAX_BRIDGES: usize = 10;
pub const MAX_USERS: usize = 20;
const IDLE: Duration = Duration::from_secs(10);
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CHARS: usize = 2000;
/// A long reply goes out as at most this many Discord messages.
const MAX_PARTS: usize = 4;

/// The bridge's settings; the bot token is in the secret store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct DiscordSettings {
    pub enabled: bool,
    /// Discord user ids whose messages reach agents; everyone else is ignored.
    pub users: Vec<String>,
    /// Each channel talks to one agent.
    pub bridges: Vec<Bridge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Bridge {
    /// The Discord channel id (a number, as text).
    pub channel_id: String,
    #[schema(value_type = String)]
    pub agent_id: AgentId,
}

/// How a bridged channel is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct ChannelStatus {
    pub channel_id: String,
    /// When the daemon last read the channel.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub checked_at: Option<OffsetDateTime>,
    /// Why the last read or reply failed; `null` when it worked.
    pub error: Option<String>,
    #[serde(skip)]
    last_id: Option<u64>,
}

/// Bridge state kept in memory: where each channel is, and which mail goes back where.
#[derive(Debug, Default)]
pub struct Discord {
    channels: Mutex<HashMap<String, ChannelStatus>>,
    /// Mail message id → (channel, Discord message id), for the reply.
    pending: Mutex<HashMap<i64, (String, String)>>,
}

impl Discord {
    pub fn status(&self) -> Vec<ChannelStatus> {
        let channels = self.channels.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out: Vec<ChannelStatus> = channels.values().cloned().collect();
        out.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));
        out
    }

    fn update(&self, channel: &str, f: impl FnOnce(&mut ChannelStatus)) {
        let mut channels = self.channels.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = channels
            .entry(channel.to_owned())
            .or_insert_with(|| ChannelStatus {
                channel_id: channel.to_owned(),
                ..ChannelStatus::default()
            });
        f(entry);
    }

    fn last_id(&self, channel: &str) -> Option<u64> {
        let channels = self.channels.lock().unwrap_or_else(PoisonError::into_inner);
        channels.get(channel).and_then(|c| c.last_id)
    }
}

pub async fn load(state: &AppState) -> Result<DiscordSettings, ApiError> {
    match state.store.get_setting(SETTING).await? {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|e| ApiError::Internal(format!("discord settings: {e}"))),
        None => Ok(DiscordSettings::default()),
    }
}

pub async fn save(state: &AppState, settings: &DiscordSettings) -> Result<(), ApiError> {
    let raw = serde_json::to_string(settings).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(state.store.put_setting(SETTING, raw).await?)
}

/// A Discord id: a snowflake, 15-21 digits.
pub fn is_snowflake(id: &str) -> bool {
    (15..=21).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

/// Reads the bridged channels every `poll` while the bridge is on; new messages from allowed
/// users go to the channel's agent. Settings changes apply on the next round.
pub async fn run(state: AppState, poll: Duration) {
    loop {
        let settings = load(&state).await.unwrap_or_default();
        if !settings.enabled || settings.bridges.is_empty() {
            tokio::time::sleep(IDLE).await;
            continue;
        }
        let Ok(Some(token)) = state.settings.secrets.get(SECRET).await else {
            for b in &settings.bridges {
                state.discord.update(&b.channel_id, |c| {
                    c.error = Some("no bot token: add it in Settings → Discord".into());
                });
            }
            tokio::time::sleep(IDLE).await;
            continue;
        };
        for bridge in &settings.bridges {
            if let Err(e) = read_channel(&state, &token, &settings, bridge).await {
                tracing::warn!(channel = %bridge.channel_id, error = %e, "discord bridge");
                state
                    .discord
                    .update(&bridge.channel_id, |c| c.error = Some(e));
            }
        }
        tokio::time::sleep(poll).await;
    }
}

fn api(state: &AppState, path: &str) -> String {
    format!("{}{path}", state.settings.discord_api.trim_end_matches('/'))
}

async fn get(state: &AppState, token: &str, path: &str) -> Result<Value, String> {
    let res = state
        .http
        .get(api(state, path))
        .header("authorization", format!("Bot {token}"))
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    let status = res.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 => "Discord refused the bot token".into(),
            403 => "the bot cannot read this channel (invite it and give it access)".into(),
            404 => "Discord knows no such channel".into(),
            429 => "Discord asked to slow down; trying again soon".into(),
            _ => format!("Discord answered {status}"),
        });
    }
    res.json()
        .await
        .map_err(|_| "Discord sent no JSON".to_owned())
}

async fn read_channel(
    state: &AppState,
    token: &str,
    settings: &DiscordSettings,
    bridge: &Bridge,
) -> Result<(), String> {
    let channel = &bridge.channel_id;
    let Some(after) = state.discord.last_id(channel) else {
        // The first read only notes where the channel is; older messages are not replayed.
        let newest = get(
            state,
            token,
            &format!("/channels/{channel}/messages?limit=1"),
        )
        .await?;
        let last = newest
            .as_array()
            .and_then(|m| m.first())
            .and_then(|m| m["id"].as_str())
            .and_then(|id| id.parse().ok())
            .unwrap_or(0);
        state.discord.update(channel, |c| {
            c.last_id = Some(last);
            c.checked_at = Some(OffsetDateTime::now_utc());
            c.error = None;
        });
        return Ok(());
    };
    let page = get(
        state,
        token,
        &format!("/channels/{channel}/messages?after={after}&limit=50"),
    )
    .await?;
    let mut messages: Vec<&Value> = page
        .as_array()
        .map(|m| m.iter().collect())
        .unwrap_or_default();
    messages.sort_by_key(|m| m["id"].as_str().and_then(|id| id.parse::<u64>().ok()));
    let mut last = after;
    for m in messages {
        let Some(id) = m["id"].as_str().and_then(|id| id.parse::<u64>().ok()) else {
            continue;
        };
        last = last.max(id);
        let author = &m["author"];
        let allowed = author["id"]
            .as_str()
            .is_some_and(|a| settings.users.iter().any(|u| u == a));
        if author["bot"].as_bool() == Some(true) || !allowed {
            continue;
        }
        let mut text = m["content"].as_str().unwrap_or_default().trim().to_owned();
        for file in m["attachments"].as_array().into_iter().flatten() {
            if let Some(url) = file["url"].as_str() {
                text.push_str("\n(attachment: ");
                text.push_str(url);
                text.push(')');
            }
        }
        if text.is_empty() {
            continue;
        }
        let who = author["username"].as_str().unwrap_or("someone");
        deliver(
            state,
            bridge,
            &id.to_string(),
            &format!("{text}\n\n(via Discord, from {who})"),
        )
        .await?;
    }
    state.discord.update(channel, |c| {
        c.last_id = Some(last);
        c.checked_at = Some(OffsetDateTime::now_utc());
        c.error = None;
    });
    Ok(())
}

async fn deliver(
    state: &AppState,
    bridge: &Bridge,
    discord_id: &str,
    text: &str,
) -> Result<(), String> {
    let mut body = state.normalize_mentions(text).await;
    if body.len() > ferrobot_mcp::MAX_MESSAGE_BYTES {
        let mut end = ferrobot_mcp::MAX_MESSAGE_BYTES;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
    }
    let msg = state
        .store
        .enqueue_message(Sender::User, bridge.agent_id, body, None)
        .await
        .map_err(|e| e.to_string())?;
    state
        .discord
        .pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(msg.id, (bridge.channel_id.clone(), discord_id.to_owned()));
    state.message_arrived(bridge.agent_id);
    state.publish(UiEvent::Message { message: msg });
    Ok(())
}

/// A turn on a message from Discord ended: its reply goes to the channel. Without a final
/// result from the runner, the turn's last reply text is used.
pub fn turn_done(state: &AppState, message: i64, turn: (AgentId, &str), result: Option<String>) {
    let Some(target) = take_pending(state, message) else {
        return;
    };
    let (agent, turn) = (turn.0, turn.1.to_owned());
    let state = state.clone();
    tokio::spawn(async move {
        let text = match result.filter(|r| !r.trim().is_empty()) {
            Some(r) => r,
            None => last_reply(&state, agent, turn).await.unwrap_or_default(),
        };
        if !text.trim().is_empty() {
            post(&state, target, &text).await;
        }
    });
}

/// A turn on a message from Discord failed for good: the channel hears so.
pub fn turn_failed(state: &AppState, message: i64) {
    let Some(target) = take_pending(state, message) else {
        return;
    };
    let state = state.clone();
    tokio::spawn(async move {
        post(
            &state,
            target,
            "(The agent could not answer; see ferrobot.)",
        )
        .await;
    });
}

fn take_pending(state: &AppState, message: i64) -> Option<(String, String)> {
    state
        .discord
        .pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&message)
}

async fn last_reply(state: &AppState, agent: AgentId, turn: String) -> Option<String> {
    let events = state.store.turn_events(agent, turn).await.ok()?;
    let text = events
        .iter()
        .rev()
        .find(|e| e.kind == "assistant_text")?
        .payload["text"]
        .as_str()?
        .to_owned();
    Some(state.render_mentions(&text).await)
}

async fn post(state: &AppState, (channel, discord_id): (String, String), text: &str) {
    {
        let Ok(Some(token)) = state.settings.secrets.get(SECRET).await else {
            return;
        };
        let parts = split(text);
        for (n, part) in parts.iter().enumerate() {
            let mut body = json!({ "content": part, "allowed_mentions": { "parse": [] } });
            if n == 0 {
                body["message_reference"] =
                    json!({ "message_id": discord_id, "fail_if_not_exists": false });
            }
            let sent = state
                .http
                .post(api(state, &format!("/channels/{channel}/messages")))
                .header("authorization", format!("Bot {token}"))
                .timeout(TIMEOUT)
                .json(&body)
                .send()
                .await;
            let error = match sent {
                Ok(r) if r.status().is_success() => None,
                Ok(r) => Some(format!(
                    "posting the reply: Discord answered {}",
                    r.status()
                )),
                Err(e) => Some(format!("posting the reply: {}", e.without_url())),
            };
            if let Some(e) = error {
                state.discord.update(&channel, |c| c.error = Some(e));
                return;
            }
        }
    }
}

/// A reply in Discord-sized parts: at most 2000 characters each, cut at a line break where
/// possible, and at most four; the rest is left out with a note.
fn split(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() && parts.len() < MAX_PARTS {
        if rest.chars().count() <= MAX_CHARS {
            parts.push(rest.to_owned());
            rest = "";
            break;
        }
        let limit = rest
            .char_indices()
            .nth(MAX_CHARS - 40)
            .map_or(rest.len(), |(i, _)| i);
        let cut = rest[..limit]
            .rfind('\n')
            .filter(|&i| i > limit / 2)
            .unwrap_or(limit);
        parts.push(rest[..cut].trim_end().to_owned());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty()
        && let Some(last) = parts.last_mut()
    {
        last.push_str("\n… (the rest is in ferrobot)");
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_replies_are_split_for_discord() {
        assert_eq!(split("hi"), ["hi"]);
        let long = format!("{}\n{}", "a".repeat(1500), "b".repeat(1500));
        let parts = split(&long);
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.chars().count() <= MAX_CHARS));
        assert_eq!(parts[0], "a".repeat(1500), "cut at the line break");
        let huge = "x".repeat(MAX_CHARS * 6);
        let parts = split(&huge);
        assert_eq!(parts.len(), MAX_PARTS);
        assert!(parts[3].ends_with("(the rest is in ferrobot)"));
    }

    #[test]
    fn ids_are_discord_snowflakes() {
        assert!(is_snowflake("123456789012345678"));
        assert!(!is_snowflake("12345") && !is_snowflake("12345678901234567x"));
    }
}
