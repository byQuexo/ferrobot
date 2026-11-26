//! Voice settings, speech, transcription and the voice call. Keys stay in the secret store and
//! are never returned; text and audio are never logged.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use ferrobot_core::{AgentId, MailMessage};
use ferrobot_store::{Room, RoomLine};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ApiResult;
use super::agents::explicit;
use super::providers::{check_key, clean, set_secret};
use crate::AppState;
use crate::call::MAX_CALL_TEXT_BYTES;
use crate::error::{ApiError, ErrorBody};
use crate::voice::{
    self, LiveMode, MAX_SPEECH_CHARS, ModelOption, Speak, Transcript, VoiceOption, VoiceProvider,
    VoiceSettings,
};

const SETTING: &str = "voice";

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct VoiceView {
    #[serde(flatten)]
    settings: VoiceSettings,
    /// Every provider with its defaults and whether its key is stored.
    providers: Vec<VoiceProviderView>,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct VoiceProviderView {
    kind: VoiceProvider,
    /// Whether an API key is stored. The key itself is never returned.
    has_key: bool,
    default_tts_model: &'static str,
    /// `null`: no default; pick a voice of your account.
    default_voice: Option<&'static str>,
    default_stt_model: &'static str,
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(format!("voice settings: {e}"))
}

pub(crate) async fn load(state: &AppState) -> Result<VoiceSettings, ApiError> {
    match state.store.get_setting(SETTING).await? {
        Some(raw) => serde_json::from_str(&raw).map_err(internal),
        None => Ok(VoiceSettings::default()),
    }
}

async fn view(state: &AppState, settings: VoiceSettings) -> Result<VoiceView, ApiError> {
    let mut providers = Vec::new();
    for kind in VoiceProvider::ALL {
        providers.push(VoiceProviderView {
            kind,
            has_key: state
                .settings
                .secrets
                .get(kind.secret_key())
                .await?
                .is_some(),
            default_tts_model: kind.default_tts_model(),
            default_voice: kind.default_voice(),
            default_stt_model: kind.default_stt_model(),
        });
    }
    Ok(VoiceView {
        settings,
        providers,
    })
}

/// Voice settings and the speech providers.
#[utoipa::path(get, path = "/v1/voice", tag = "voice",
    responses((status = 200, body = VoiceView)))]
pub(crate) async fn get_voice(State(state): State<AppState>) -> ApiResult<VoiceView> {
    let settings = load(&state).await?;
    Ok(Json(view(&state, settings).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[allow(clippy::option_option, reason = "omitted vs. explicit null")]
pub(crate) struct UpdateVoice {
    #[serde(flatten)]
    settings: VoiceSettings,
    /// New API key for `provider`; `null` removes it, omitted keeps it. Kept in the
    /// daemon's secret store; never returned.
    #[serde(default, deserialize_with = "explicit")]
    #[schema(value_type = Option<String>)]
    api_key: Option<Option<String>>,
}

/// Replaces the voice settings; with `api_key`, also stores or removes the
/// key of `provider`.
#[utoipa::path(put, path = "/v1/voice", tag = "voice",
    request_body = UpdateVoice,
    responses((status = 200, body = VoiceView),
        (status = 400, body = ErrorBody)))]
pub(crate) async fn put_voice(
    State(state): State<AppState>,
    Json(req): Json<UpdateVoice>,
) -> ApiResult<VoiceView> {
    let settings = VoiceSettings {
        enabled: req.settings.enabled,
        speed: req.settings.speed,
        language: clean(req.settings.language).map(|l| l.to_ascii_lowercase()),
        provider: req.settings.provider,
        tts_model: clean(req.settings.tts_model),
        voice: clean(req.settings.voice),
        stt_model: clean(req.settings.stt_model),
        agent_voices: req
            .settings
            .agent_voices
            .into_iter()
            .filter_map(|(agent, voice)| Some((agent, clean(Some(voice))?)))
            .collect(),
    };
    settings.validate().map_err(ApiError::BadRequest)?;
    let key = req.api_key.map(clean);
    if let Some(Some(key)) = &key {
        check_key(key)?;
    }
    let provider = match (settings.provider, &key) {
        (None, Some(_)) => return Err(ApiError::BadRequest("api_key needs a provider".into())),
        (provider, _) => provider,
    };
    let json = serde_json::to_string(&settings).map_err(internal)?;
    state.store.put_setting(SETTING, json).await?;
    if let (Some(provider), Some(key)) = (provider, key) {
        set_secret(&state, provider.secret_key(), key).await?;
    }
    Ok(Json(view(&state, settings).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct VoiceKey {
    /// The new key; `null` removes it. Kept in the daemon's secret store; never returned.
    api_key: Option<String>,
}

/// Stores or removes one speech provider's key without changing which provider is used.
#[utoipa::path(put, path = "/v1/voice/keys/{provider}", tag = "voice",
    params(("provider" = VoiceProvider, Path, description = "Speech provider")),
    request_body = VoiceKey,
    responses((status = 200, body = VoiceView),
        (status = 400, body = ErrorBody),
        (status = 404, description = "Unknown provider", body = ErrorBody)))]
pub(crate) async fn put_voice_key(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(req): Json<VoiceKey>,
) -> ApiResult<VoiceView> {
    let provider: VoiceProvider = serde_json::from_value(serde_json::Value::String(provider))
        .map_err(|_| ApiError::NotFound("unknown speech provider".into()))?;
    set_secret(&state, provider.secret_key(), req.api_key).await?;
    let settings = load(&state).await?;
    Ok(Json(view(&state, settings).await?))
}

/// The configured provider, its key and where to reach it.
struct Ready {
    provider: VoiceProvider,
    settings: VoiceSettings,
    key: String,
    base: String,
}

async fn ready(state: &AppState) -> Result<Ready, ApiError> {
    let settings = load(state).await?;
    if !settings.enabled {
        return Err(ApiError::Conflict(
            "voice is switched off in the settings".into(),
        ));
    }
    let provider = settings
        .provider
        .ok_or_else(|| ApiError::Conflict("voice is not set up".into()))?;
    let key = state
        .settings
        .secrets
        .get(provider.secret_key())
        .await?
        .ok_or_else(|| ApiError::Conflict("no API key stored for the voice provider".into()))?;
    let base = state
        .settings
        .voice_upstream
        .clone()
        .unwrap_or_else(|| provider.base_url().to_owned());
    Ok(Ready {
        provider,
        settings,
        key,
        base,
    })
}

fn spend(state: &AppState, chars: usize, transcriptions: u32) -> Result<(), ApiError> {
    let mut budget = state
        .voice
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if budget.spend(chars, transcriptions) {
        Ok(())
    } else {
        Err(ApiError::TooManyRequests(
            "voice limit for this minute reached".into(),
        ))
    }
}

async fn send(request: reqwest::RequestBuilder) -> Result<reqwest::Response, ApiError> {
    let response = request.send().await.map_err(|e| {
        tracing::warn!(error = %e, "voice provider unreachable");
        ApiError::Upstream("voice provider unreachable".into())
    })?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    tracing::warn!(%status, "voice provider refused a request");
    // The provider's reason (unknown voice, no credits) goes to the operator only.
    let reason: String = response
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect();
    Err(ApiError::Upstream(format!(
        "voice provider answered {status}: {reason}"
    )))
}

#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct SpeechRequest {
    /// What to say: 1-4096 characters.
    text: String,
    /// Speak with this agent's voice, if it has one.
    #[serde(default)]
    agent_id: Option<AgentId>,
}

/// Speaks `text` with the configured provider and voice. The audio
/// (usually `audio/mpeg`) streams while the provider makes it.
#[utoipa::path(post, path = "/v1/voice/speech", tag = "voice",
    request_body = SpeechRequest,
    responses(
        (status = 200, description = "Audio, streamed", content_type = "audio/mpeg", body = Vec<u8>),
        (status = 400, description = "Empty or too long", body = ErrorBody),
        (status = 409, description = "Voice off, not set up, or no key or voice", body = ErrorBody),
        (status = 429, description = "Limit for this minute reached", body = ErrorBody),
        (status = 502, description = "The provider failed", body = ErrorBody)))]
pub(crate) async fn speech(
    State(state): State<AppState>,
    Json(req): Json<SpeechRequest>,
) -> Result<Response, ApiError> {
    speak(&state, &req).await
}

/// The same speech as a link, so an audio element can play it while it streams (the operator
/// token goes in `access_token`, as for other `GET` links).
#[utoipa::path(get, path = "/v1/voice/speech", tag = "voice",
    params(SpeechRequest),
    responses(
        (status = 200, description = "Audio, streamed", content_type = "audio/mpeg", body = Vec<u8>),
        (status = 400, description = "Empty or too long", body = ErrorBody),
        (status = 409, description = "Voice off, not set up, or no key or voice", body = ErrorBody),
        (status = 429, description = "Limit for this minute reached", body = ErrorBody),
        (status = 502, description = "The provider failed", body = ErrorBody)))]
pub(crate) async fn speech_link(
    State(state): State<AppState>,
    axum::extract::Query(req): axum::extract::Query<SpeechRequest>,
) -> Result<Response, ApiError> {
    speak(&state, &req).await
}

async fn speak(state: &AppState, req: &SpeechRequest) -> Result<Response, ApiError> {
    let text = req.text.trim();
    let chars = text.chars().count();
    if chars == 0 || chars > MAX_SPEECH_CHARS {
        return Err(ApiError::BadRequest(format!(
            "text must be 1-{MAX_SPEECH_CHARS} characters"
        )));
    }
    let r = ready(state).await?;
    let in_call = match req.agent_id {
        Some(agent) => call_voice(state, &r, agent).await,
        None => None,
    };
    let voice = in_call
        .as_deref()
        .or_else(|| r.settings.voice_of(r.provider, req.agent_id))
        .ok_or_else(|| ApiError::Conflict("pick a voice in the voice settings".into()))?;
    let model = r
        .settings
        .tts_model
        .as_deref()
        .unwrap_or(r.provider.default_tts_model());
    spend(state, chars, 0)?;
    let speak = Speak {
        model,
        voice,
        text,
        speed: r.settings.speed,
        language: r.settings.language.as_deref(),
    };
    let request = voice::speech(&state.http, &r.base, r.provider, &r.key, speak);
    let response = send(request).await?;
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .filter(|v| v.as_bytes().starts_with(b"audio/"))
        .cloned()
        .unwrap_or(HeaderValue::from_static("audio/mpeg"));
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from_stream(response.bytes_stream()))
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// Transcribes a recording: the raw audio as the body, its type as
/// `Content-Type`.
#[utoipa::path(post, path = "/v1/voice/transcription", tag = "voice",
    request_body(content = Vec<u8>, content_type = "audio/*",
        description = "webm, ogg, mp4/m4a, mp3, wav or flac; at most 10 MiB"),
    responses((status = 200, body = Transcript),
        (status = 400, description = "Empty or unsupported audio", body = ErrorBody),
        (status = 409, description = "Voice off, not set up or no key", body = ErrorBody),
        (status = 413, description = "Over 10 MiB"),
        (status = 429, description = "Limit for this minute reached", body = ErrorBody),
        (status = 502, description = "The provider failed", body = ErrorBody)))]
pub(crate) async fn transcription(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Transcript> {
    if body.is_empty() {
        return Err(ApiError::BadRequest("no audio".into()));
    }
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let r = ready(&state).await?;
    let model = r
        .settings
        .stt_model
        .as_deref()
        .unwrap_or(r.provider.default_stt_model());
    let listen = voice::Listen {
        model,
        language: r.settings.language.as_deref(),
    };
    let request = voice::transcription(
        &state.http,
        &r.base,
        r.provider,
        &r.key,
        listen,
        mime,
        &body,
    )
    .map_err(ApiError::BadRequest)?;
    spend(&state, 0, 1)?;
    let transcript: Transcript = send(request)
        .await?
        .json()
        .await
        .map_err(|_| ApiError::Upstream("the voice provider sent no transcript".into()))?;
    Ok(Json(Transcript {
        text: transcript.text.trim().to_owned(),
    }))
}

#[derive(Debug, Default, Serialize, ToSchema)]
pub(crate) struct VoiceCatalog {
    /// The account's voices (`ElevenLabs`); empty for other providers.
    voices: Vec<VoiceOption>,
    /// Text-to-speech models of the account (`ElevenLabs`); empty otherwise.
    tts_models: Vec<ModelOption>,
}

/// Most voices fetched (pages of 100), so a huge library stays quick.
const MAX_VOICE_PAGES: usize = 3;

/// The `ElevenLabs` account's voices, fetched live.
async fn account_voices(state: &AppState, r: &Ready) -> Result<Vec<VoiceOption>, ApiError> {
    let base = r.base.trim_end_matches('/');
    let json = |response: reqwest::Response| async move {
        response
            .json::<serde_json::Value>()
            .await
            .map_err(|_| ApiError::Upstream("the voice provider sent no list".into()))
    };
    let mut voices = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_VOICE_PAGES {
        let mut url = url::Url::parse(&format!("{base}/v2/voices"))
            .map_err(|e| ApiError::Internal(format!("voice upstream: {e}")))?;
        url.query_pairs_mut()
            .append_pair("page_size", "100")
            .append_pair("include_total_count", "false");
        if let Some(c) = &cursor {
            url.query_pairs_mut().append_pair("next_page_token", c);
        }
        let request = state
            .http
            .get(url)
            .header("xi-api-key", &r.key)
            .timeout(voice::TIMEOUT);
        let (page, next) = voice::parse_voices(&json(send(request).await?).await?);
        voices.extend(page);
        cursor = next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(voices)
}

/// How long the account's voice list is reused for picking call voices.
const VOICE_POOL_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// The last voice list: (provider address, when, voice ids).
type Pool = Option<(String, std::time::Instant, Vec<String>)>;

static VOICE_POOL: std::sync::Mutex<Pool> = std::sync::Mutex::new(None);

/// The account's voice ids, by name, cached for [`VOICE_POOL_TTL`]; empty when not listable.
async fn voice_pool(state: &AppState, r: &Ready) -> Vec<String> {
    if r.provider != VoiceProvider::Elevenlabs {
        return Vec::new();
    }
    let fresh = |c: &Pool| {
        c.as_ref()
            .filter(|(base, at, _)| *base == r.base && at.elapsed() < VOICE_POOL_TTL)
            .map(|(_, _, ids)| ids.clone())
    };
    if let Some(ids) = fresh(
        &VOICE_POOL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    ) {
        return ids;
    }
    let Ok(mut voices) = account_voices(state, r).await else {
        return Vec::new();
    };
    voices.sort_by_key(|v| v.name.to_lowercase());
    let ids: Vec<String> = voices.into_iter().map(|v| v.voice_id).collect();
    *VOICE_POOL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some((r.base.clone(), std::time::Instant::now(), ids.clone()));
    ids
}

/// The `place`-th voice of `pool` that nobody in the call speaks with yet (`taken`).
fn distinct_voice(place: usize, pool: &[String], taken: &[&str]) -> Option<String> {
    pool.iter()
        .filter(|v| !taken.contains(&v.as_str()))
        .nth(place.checked_sub(1)?)
        .cloned()
}

/// In a call with several agents, one without a voice of its own speaks with a voice no other
/// agent in the call uses, by its place in the call; the host keeps the default.
async fn call_voice(state: &AppState, r: &Ready, agent: AgentId) -> Option<String> {
    let (agents, _) = state.call_view().await;
    let place = agents.iter().position(|a| *a == agent)?;
    if place == 0 || r.settings.agent_voices.contains_key(&agent) {
        return None;
    }
    let taken: Vec<&str> = agents
        .iter()
        .filter(|a| **a == agents[0] || r.settings.agent_voices.contains_key(a))
        .filter_map(|a| r.settings.voice_of(r.provider, Some(*a)))
        .collect();
    distinct_voice(place, &voice_pool(state, r).await, &taken)
}

/// The voices and speech models of the configured provider, fetched live.
#[utoipa::path(get, path = "/v1/voice/catalog", tag = "voice",
    responses((status = 200, body = VoiceCatalog),
        (status = 409, description = "Voice off, not set up or no key", body = ErrorBody),
        (status = 502, description = "The provider failed", body = ErrorBody)))]
pub(crate) async fn catalog(State(state): State<AppState>) -> ApiResult<VoiceCatalog> {
    let r = ready(&state).await?;
    if r.provider != VoiceProvider::Elevenlabs {
        return Ok(Json(VoiceCatalog::default()));
    }
    let base = r.base.trim_end_matches('/');
    let json = |response: reqwest::Response| async move {
        response
            .json::<serde_json::Value>()
            .await
            .map_err(|_| ApiError::Upstream("the voice provider sent no list".into()))
    };
    let mut voices = account_voices(&state, &r).await?;
    voices.sort_by_key(|v| v.name.to_lowercase());
    let models = state
        .http
        .get(format!("{base}/v1/models"))
        .header("xi-api-key", &r.key)
        .timeout(voice::TIMEOUT);
    let tts_models = voice::parse_models(&json(send(models).await?).await?);
    Ok(Json(VoiceCatalog { voices, tts_models }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct LiveRequest {
    mode: LiveMode,
    /// Sample rate of the 16-bit mono PCM the app sends: 8000, 16000,
    /// 22050, 24000, 44100 or 48000.
    sample_rate: u32,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct LiveSession {
    /// The provider's WebSocket with a single-use token (valid 15 minutes,
    /// one session of at most 10 minutes); the real key never leaves the daemon.
    url: String,
}

/// Opens live transcription for dictation or a hands-free call
/// (`ElevenLabs` only; other providers transcribe after recording).
#[utoipa::path(post, path = "/v1/voice/live", tag = "voice",
    request_body = LiveRequest,
    responses((status = 200, body = LiveSession),
        (status = 400, description = "Unsupported sample rate", body = ErrorBody),
        (status = 409, description = "Voice off, not set up, no key, or not ElevenLabs", body = ErrorBody),
        (status = 429, description = "Limit for this minute reached", body = ErrorBody),
        (status = 502, description = "The provider failed", body = ErrorBody)))]
pub(crate) async fn live(
    State(state): State<AppState>,
    Json(req): Json<LiveRequest>,
) -> ApiResult<LiveSession> {
    let r = ready(&state).await?;
    if r.provider != VoiceProvider::Elevenlabs {
        return Err(ApiError::Conflict(
            "live transcription needs ElevenLabs".into(),
        ));
    }
    // Fails before the provider is asked for a token.
    voice::live_url(&r.base, "-", req.mode, req.sample_rate, None, &[])
        .map_err(ApiError::BadRequest)?;
    // Agent names are what generic transcription gets wrong most ("builder" → "Biller").
    let agents = state.store.list_agents().await?;
    let keyterms = voice::keyterms(agents.iter().map(|a| a.name.as_str()));
    spend(&state, 0, 1)?;
    let token: serde_json::Value = send(voice::live_token(&state.http, &r.base, &r.key))
        .await?
        .json()
        .await
        .map_err(|_| ApiError::Upstream("the voice provider sent no token".into()))?;
    let token = token["token"]
        .as_str()
        .ok_or_else(|| ApiError::Upstream("the voice provider sent no token".into()))?;
    let url = voice::live_url(
        &r.base,
        token,
        req.mode,
        req.sample_rate,
        r.settings.language.as_deref(),
        &keyterms,
    )
    .map_err(ApiError::BadRequest)?;
    Ok(Json(LiveSession { url }))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub(crate) struct CallView {
    /// Agents in the operator's voice call (at most 4); empty: no call.
    agents: Vec<AgentId>,
    /// The call's room, once a second agent joined; ignored when sent.
    #[serde(default)]
    #[schema(read_only)]
    room_id: Option<i64>,
}

pub(crate) async fn call_view(state: &AppState) -> CallView {
    let (agents, room_id) = state.call_view().await;
    CallView { agents, room_id }
}

/// Who is in the operator's voice call.
#[utoipa::path(get, path = "/v1/call", tag = "voice",
    responses((status = 200, body = CallView)))]
pub(crate) async fn get_call(State(state): State<AppState>) -> Json<CallView> {
    Json(call_view(&state).await)
}

/// Starts, changes or (with no agents) ends the voice call. Agents in it can invite others
/// with their `invite_to_call` tool. With two or more agents the call moves into a room.
#[utoipa::path(put, path = "/v1/call", tag = "voice",
    request_body = CallView,
    responses((status = 200, body = CallView),
        (status = 400, description = "More than 4 agents", body = ErrorBody),
        (status = 404, description = "Unknown agent", body = ErrorBody),
        (status = 409, description = "Voice is switched off", body = ErrorBody)))]
pub(crate) async fn put_call(
    State(state): State<AppState>,
    Json(req): Json<CallView>,
) -> ApiResult<CallView> {
    if !req.agents.is_empty() && !load(&state).await?.enabled {
        return Err(ApiError::Conflict(
            "voice is switched off in the settings".into(),
        ));
    }
    for agent in &req.agents {
        state.store.get_agent(*agent).await?;
    }
    state
        .set_call(req.agents)
        .await
        .map_err(ApiError::BadRequest)?;
    Ok(Json(call_view(&state).await))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CallMessage {
    /// 1..19316 bytes after trimming, counting `@name` mentions as their
    /// stored `<@agent-id>` tokens (the rest of a message is left for what
    /// the others said).
    body: String,
}

/// Sends the operator's message to the call with the call instructions. Without a room every
/// agent gets it as a turn. In a room it is a line, and the agents take it one at a time, host
/// first, each with what it has not heard yet; their messages (`room_id` set) stay out of chats.
#[utoipa::path(post, path = "/v1/call/messages", tag = "voice",
    request_body = CallMessage,
    responses(
        (status = 202, description = "Queued: one message per agent, or in a room the next \
            speaker's (none while an agent still speaks)", body = Vec<MailMessage>),
        (status = 400, body = ErrorBody),
        (status = 409, description = "No voice call", body = ErrorBody)))]
pub(crate) async fn post_call_message(
    State(state): State<AppState>,
    Json(req): Json<CallMessage>,
) -> Result<(StatusCode, Json<Vec<MailMessage>>), ApiError> {
    let body = state.normalize_mentions(req.body.trim()).await;
    if body.is_empty() || body.len() > MAX_CALL_TEXT_BYTES {
        return Err(ApiError::BadRequest(format!(
            "body must be 1..{MAX_CALL_TEXT_BYTES} bytes"
        )));
    }
    let sent = state
        .say_in_call(&body)
        .await
        .ok_or_else(|| ApiError::Conflict("there is no voice call".into()))?;
    Ok((StatusCode::ACCEPTED, Json(sent)))
}

/// A room with what was said in it.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct RoomTranscript {
    #[serde(flatten)]
    room: Room,
    /// Oldest first; the last 1000.
    lines: Vec<RoomLine>,
    /// The member whose turn it is (writing or speaking), if any; `room_floor` events follow it.
    speaking: Option<AgentId>,
}

/// Call rooms (calls with two or more agents), newest first; the last 200.
#[utoipa::path(get, path = "/v1/rooms", tag = "voice",
    responses((status = 200, body = Vec<Room>)))]
pub(crate) async fn list_rooms(State(state): State<AppState>) -> ApiResult<Vec<Room>> {
    Ok(Json(state.store.rooms(200).await?))
}

/// One call room or group: who is in it and what was said.
#[utoipa::path(get, path = "/v1/rooms/{id}", tag = "voice",
    params(("id" = i64, Path, description = "Room id")),
    responses((status = 200, body = RoomTranscript),
        (status = 404, description = "Unknown room", body = ErrorBody)))]
pub(crate) async fn get_room(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<RoomTranscript> {
    Ok(Json(RoomTranscript {
        room: state.store.room(id).await?,
        lines: state.store.room_lines(id, 1000).await?,
        speaking: state.floor_of(id).await,
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn call_agents_without_a_voice_get_distinct_ones() {
        let pool: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        assert_eq!(
            super::distinct_voice(0, &pool, &[]),
            None,
            "the host keeps the default"
        );
        assert_eq!(
            super::distinct_voice(1, &pool, &["a"]).as_deref(),
            Some("b")
        );
        assert_eq!(
            super::distinct_voice(2, &pool, &["a"]).as_deref(),
            Some("c")
        );
        assert_eq!(
            super::distinct_voice(3, &pool, &["a"]),
            None,
            "out of voices: the default"
        );
    }
}
