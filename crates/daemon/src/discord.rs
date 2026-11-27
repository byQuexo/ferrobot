//! The Discord bridge: allowed users' messages in a bridged channel reach its agent as the
//! operator's and replies go back. Polled with the bot token, which never leaves the daemon.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use ferrobot_core::{AgentId, Sender};
use ferrobot_store::StoredEvent;
use reqwest::{Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::api::settings::{load_json, save_json};
use crate::error::ApiError;
use crate::{AppState, UiEvent};

mod cards;
pub(crate) use cards::question;

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
/// The bot's reactions on a bridged message: seen once it reached the agent, done once answered.
const SEEN: &str = "👀";
const DONE: &str = "✅";

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
    /// Why the latest new messages were left out (a user not in the list, an empty message).
    pub note: Option<String>,
    #[serde(skip)]
    last_id: Option<u64>,
}

/// Bridge state kept in memory: where each channel is, and which mail goes back where.
#[derive(Debug, Default)]
pub struct Discord {
    channels: Mutex<HashMap<String, ChannelStatus>>,
    /// Mail message id → (channel, Discord message id), for the reply.
    pending: Mutex<HashMap<i64, (String, String)>>,
    /// Cards posted to channels and still waiting for an answer there.
    cards: Mutex<Vec<cards::Posted>>,
    /// Discord message id → the emoji the agent answered it with.
    reacted: Mutex<HashMap<String, String>>,
}

impl Discord {
    pub fn status(&self) -> Vec<ChannelStatus> {
        let mut out: Vec<ChannelStatus> = lock(&self.channels).values().cloned().collect();
        out.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));
        out
    }

    fn update(&self, channel: &str, f: impl FnOnce(&mut ChannelStatus)) {
        let mut channels = lock(&self.channels);
        let entry = channels
            .entry(channel.to_owned())
            .or_insert_with(|| ChannelStatus {
                channel_id: channel.to_owned(),
                ..ChannelStatus::default()
            });
        f(entry);
    }

    fn last_id(&self, channel: &str) -> Option<u64> {
        lock(&self.channels).get(channel).and_then(|c| c.last_id)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub async fn load(state: &AppState) -> Result<DiscordSettings, ApiError> {
    load_json(state, SETTING, "discord settings").await
}

pub async fn save(state: &AppState, settings: &DiscordSettings) -> Result<(), ApiError> {
    save_json(state, SETTING, settings).await
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

/// A request to Discord's API with the bot token.
fn request(state: &AppState, method: Method, token: &str, path: &str) -> RequestBuilder {
    let base = state.settings.discord_api.trim_end_matches('/');
    state
        .http
        .request(method, format!("{base}{path}"))
        .header("authorization", format!("Bot {token}"))
        .timeout(TIMEOUT)
}

/// A message body that pings nobody.
fn content(text: &str) -> Value {
    json!({ "content": text, "allowed_mentions": { "parse": [] } })
}

async fn get(state: &AppState, token: &str, path: &str) -> Result<Value, String> {
    let res = request(state, Method::GET, token, path)
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

/// The first read only notes where the channel is; older messages are not replayed.
async fn first_read(state: &AppState, token: &str, channel: &str) -> Result<(), String> {
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
    Ok(())
}

async fn read_channel(
    state: &AppState,
    token: &str,
    settings: &DiscordSettings,
    bridge: &Bridge,
) -> Result<(), String> {
    let channel = &bridge.channel_id;
    let Some(after) = state.discord.last_id(channel) else {
        return first_read(state, token, channel).await;
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
    // Why messages were left out, for the channel's status: a silent skip makes setup guesswork.
    let mut note: Option<String> = None;
    let (seen, mut delivered) = (messages.len(), 0);
    for m in messages {
        let Some(id) = m["id"].as_str().and_then(|id| id.parse::<u64>().ok()) else {
            continue;
        };
        last = last.max(id);
        let author = &m["author"];
        let allowed = author["id"]
            .as_str()
            .is_some_and(|a| settings.users.iter().any(|u| u == a));
        if author["bot"].as_bool() == Some(true) {
            continue;
        }
        if !allowed {
            let who = author["id"].as_str().unwrap_or("unknown");
            note = Some(format!(
                "Ignored a message from Discord user {who}: add that ID under Your Discord user IDs if it is you."
            ));
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
            note = Some(
                "A message arrived empty: turn on Message Content Intent on the bot's page in Discord's developer portal.".into(),
            );
            continue;
        }
        let who = author["username"].as_str().unwrap_or("someone");
        if let Some(card) = cards::replied(state, m) {
            cards::answer_reply(state, token, card, &text, who).await;
            continue;
        }
        let id = id.to_string();
        // Before delivery, so a fast answer's ✅ cannot land before the 👀.
        if let Err(e) = react(state, token, (channel, &id), SEEN, true).await {
            note = Some(e);
        }
        deliver(
            state,
            bridge,
            &id,
            &format!("{text}\n\n(via Discord, from {who})"),
        )
        .await?;
        delivered += 1;
    }
    cards::check(state, token, &settings.users, channel).await;
    if seen > 0 {
        tracing::info!(%channel, seen, delivered, skipped = ?note, "discord bridge read new messages");
    }
    state.discord.update(channel, |c| {
        c.last_id = Some(last);
        c.checked_at = Some(OffsetDateTime::now_utc());
        c.error = None;
        // A skip note stays until the next round brings something new.
        if seen > 0 {
            c.note = note;
        }
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
    crate::webhooks::truncate(&mut body, ferrobot_mcp::MAX_MESSAGE_BYTES);
    let msg = state
        .store
        .enqueue_message(Sender::User, bridge.agent_id, body, None)
        .await
        .map_err(|e| e.to_string())?;
    lock(&state.discord.pending).insert(msg.id, (bridge.channel_id.clone(), discord_id.to_owned()));
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
        let Ok(Some(token)) = state.settings.secrets.get(SECRET).await else {
            return;
        };
        let text = match result.filter(|r| !r.trim().is_empty()) {
            Some(r) => r,
            None => last_reply(&state, agent, turn).await.unwrap_or_default(),
        };
        let only_reacted = take_reacted(&state, &target.1) && text.trim().is_empty();
        if text.trim().is_empty() || post(&state, &token, &target.0, Some(&target.1), &text).await {
            // An emoji alone is the whole answer: no ✅ beside it.
            settle(&state, &token, &target, !only_reacted).await;
        }
    });
}

/// The agent reacted to a message from Discord: the same emoji goes on it there, replacing
/// the agent's earlier one.
pub fn reaction(state: &AppState, event: &StoredEvent) {
    let (Some(message), Some(emoji)) = (
        event.payload["message_id"].as_i64(),
        event.payload["emoji"].as_str(),
    ) else {
        return;
    };
    let Some((channel, id)) = lock(&state.discord.pending).get(&message).cloned() else {
        return;
    };
    // Recorded before the turn can end, so its ✅ is left out.
    let earlier = lock(&state.discord.reacted).insert(id.clone(), emoji.to_owned());
    let (state, emoji) = (state.clone(), emoji.to_owned());
    tokio::spawn(async move {
        let Ok(Some(token)) = state.settings.secrets.get(SECRET).await else {
            return;
        };
        if let Some(old) = earlier.filter(|old| *old != emoji) {
            let _ = react(&state, &token, (&channel, &id), &old, false).await;
        }
        if let Err(e) = react(&state, &token, (&channel, &id), &emoji, true).await {
            state.discord.update(&channel, |c| c.note = Some(e));
        }
    });
}

fn take_reacted(state: &AppState, discord_id: &str) -> bool {
    lock(&state.discord.reacted).remove(discord_id).is_some()
}

/// A turn on a message from Discord failed for good: the channel hears so.
pub fn turn_failed(state: &AppState, message: i64) {
    let Some(target) = take_pending(state, message) else {
        return;
    };
    let state = state.clone();
    tokio::spawn(async move {
        let Ok(Some(token)) = state.settings.secrets.get(SECRET).await else {
            return;
        };
        post(
            &state,
            &token,
            &target.0,
            Some(&target.1),
            "(The agent could not answer; see ferrobot.)",
        )
        .await;
        take_reacted(&state, &target.1);
        settle(&state, &token, &target, false).await;
    });
}

/// The channels bridged to `agent` while the bridge is on.
pub async fn channels_of(state: &AppState, agent: AgentId) -> Vec<String> {
    let settings = load(state).await.unwrap_or_default();
    if !settings.enabled {
        return Vec::new();
    }
    settings
        .bridges
        .into_iter()
        .filter(|b| b.agent_id == agent)
        .map(|b| b.channel_id)
        .collect()
}

/// `agent` posts `text` on its own to its bridged channel (`channel`: which one, when it has
/// several); only channels bridged to it are reachable. `Ok` is the channel id.
pub async fn send(
    state: &AppState,
    agent: AgentId,
    text: &str,
    channel: Option<String>,
) -> Result<String, String> {
    let mine = channels_of(state, agent).await;
    let channel = match (channel, mine.as_slice()) {
        (_, []) => return Err("no Discord channel is bridged to you".into()),
        (None, [only]) => only.clone(),
        (Some(c), _) if mine.contains(&c) => c,
        (None, _) => {
            return Err(format!(
                "several Discord channels are bridged to you; pass channel_id, one of: {}",
                mine.join(", ")
            ));
        }
        (Some(c), _) => {
            return Err(format!(
                "channel {c} is not bridged to you; yours: {}",
                mine.join(", ")
            ));
        }
    };
    let Ok(Some(token)) = state.settings.secrets.get(SECRET).await else {
        return Err(
            "the Discord bot token is missing; your operator adds it in Settings → Discord".into(),
        );
    };
    if post(state, &token, &channel, None, text).await {
        Ok(channel)
    } else {
        Err("Discord refused the message; your operator sees why in Settings → Discord".into())
    }
}

fn take_pending(state: &AppState, message: i64) -> Option<(String, String)> {
    lock(&state.discord.pending).remove(&message)
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

/// Posts `text`, the first part as a reply to `reply_to` when given; false if Discord refused.
async fn post(
    state: &AppState,
    token: &str,
    channel: &str,
    reply_to: Option<&str>,
    text: &str,
) -> bool {
    for (n, part) in split(text).iter().enumerate() {
        let mut body = content(part);
        if let (0, Some(id)) = (n, reply_to) {
            body["message_reference"] = json!({ "message_id": id, "fail_if_not_exists": false });
        }
        let path = format!("/channels/{channel}/messages");
        let sent = request(state, Method::POST, token, &path)
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
            state.discord.update(channel, |c| c.error = Some(e));
            return false;
        }
    }
    true
}

/// Swaps the 👀 on the operator's message for ✅ once answered, or just takes it off.
async fn settle(
    state: &AppState,
    token: &str,
    (channel, message): &(String, String),
    answered: bool,
) {
    let steps: &[(&str, bool)] = if answered {
        &[(DONE, true), (SEEN, false)]
    } else {
        &[(SEEN, false)]
    };
    for &(emoji, add) in steps {
        if let Err(e) = react(state, token, (channel, message), emoji, add).await {
            state.discord.update(channel, |c| c.note = Some(e));
            return;
        }
    }
}

/// Adds or removes the bot's own reaction; the error is the channel note to show.
async fn react(
    state: &AppState,
    token: &str,
    (channel, message): (&str, &str),
    emoji: &str,
    add: bool,
) -> Result<(), String> {
    // Percent-encoded byte by byte, so the emoji stays one path segment whatever it holds.
    let emoji: String = emoji.bytes().map(|b| format!("%{b:02X}")).collect();
    let path = format!("/channels/{channel}/messages/{message}/reactions/{emoji}/@me");
    let method = if add { Method::PUT } else { Method::DELETE };
    let status = match request(state, method, token, &path)
        .header("content-length", "0")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => return Ok(()),
        Ok(r) => r.status().to_string(),
        Err(e) => e.without_url().to_string(),
    };
    Err(format!(
        "Could not react to a message ({status}): give the bot the Add Reactions permission."
    ))
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
