//! Voice for the operator's chat: settings, speech and transcription. Keys
//! stay in the daemon's secret store and are never returned; neither text
//! nor audio is logged.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ApiResult;
use super::agents::explicit;
use super::providers::{check_key, clean};
use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::voice::{self, MAX_SPEECH_CHARS, Transcript, VoiceProvider, VoiceSettings};

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

async fn load(state: &AppState) -> Result<VoiceSettings, ApiError> {
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
    /// New API key for `provider`; `null` removes it, omitted keeps it.
    /// Stored in the OS keychain; never returned.
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
        provider: req.settings.provider,
        tts_model: clean(req.settings.tts_model),
        voice: clean(req.settings.voice),
        stt_model: clean(req.settings.stt_model),
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
        let secrets = &state.settings.secrets;
        match key {
            Some(key) => secrets.set(provider.secret_key(), &key).await?,
            None => secrets.delete(provider.secret_key()).await?,
        }
    }
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

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct SpeechRequest {
    /// What to say: 1-4096 characters.
    text: String,
}

/// Speaks `text` with the configured provider and voice. The audio
/// (usually `audio/mpeg`) streams while the provider makes it.
#[utoipa::path(post, path = "/v1/voice/speech", tag = "voice",
    request_body = SpeechRequest,
    responses(
        (status = 200, description = "Audio, streamed", content_type = "audio/mpeg", body = Vec<u8>),
        (status = 400, description = "Empty or too long", body = ErrorBody),
        (status = 409, description = "Voice not set up, or no key or voice", body = ErrorBody),
        (status = 429, description = "Limit for this minute reached", body = ErrorBody),
        (status = 502, description = "The provider failed", body = ErrorBody)))]
pub(crate) async fn speech(
    State(state): State<AppState>,
    Json(req): Json<SpeechRequest>,
) -> Result<Response, ApiError> {
    let text = req.text.trim();
    let chars = text.chars().count();
    if chars == 0 || chars > MAX_SPEECH_CHARS {
        return Err(ApiError::BadRequest(format!(
            "text must be 1-{MAX_SPEECH_CHARS} characters"
        )));
    }
    let r = ready(&state).await?;
    let voice = r
        .settings
        .voice
        .as_deref()
        .or(r.provider.default_voice())
        .ok_or_else(|| ApiError::Conflict("pick a voice in the voice settings".into()))?;
    let model = r
        .settings
        .tts_model
        .as_deref()
        .unwrap_or(r.provider.default_tts_model());
    spend(&state, chars, 0)?;
    let request = voice::speech(&state.http, &r.base, r.provider, &r.key, model, voice, text);
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
        (status = 409, description = "Voice not set up or no key", body = ErrorBody),
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
    let request =
        voice::transcription(&state.http, &r.base, r.provider, &r.key, model, mime, &body)
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
