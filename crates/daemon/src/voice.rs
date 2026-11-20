//! Speech and transcription for the operator's chat with `ElevenLabs` or `OpenRouter`. The key
//! stays in the secret store (`voice:<provider>`); key, text and audio are never logged.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use ferrobot_core::AgentId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Longest text one speech request may carry (characters).
pub const MAX_SPEECH_CHARS: usize = 4096;
/// Largest recording one transcription request may carry.
pub const MAX_AUDIO_BYTES: usize = 10 * 1024 * 1024;
/// About four times as fast as anyone can listen, so a stuck UI stays cheap.
const SPEECH_CHARS_PER_MINUTE: usize = 6000;
const TRANSCRIPTIONS_PER_MINUTE: u32 = 20;
/// Provider calls end after this, streaming included.
pub const TIMEOUT: Duration = Duration::from_secs(120);
/// `ElevenLabs`' realtime transcription model for live dictation and calls.
pub const LIVE_STT_MODEL: &str = "scribe_v3_live";
/// A live session ends after this, so a forgotten microphone stays cheap.
const LIVE_MAX_SECONDS: u32 = 600;
/// Speaking speeds `ElevenLabs` offers (its own UI range).
pub const SPEEDS: std::ops::RangeInclusive<f32> = 0.7..=1.2;

/// A speech and transcription provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VoiceProvider {
    Elevenlabs,
    Openrouter,
}

impl VoiceProvider {
    pub const ALL: [Self; 2] = [Self::Elevenlabs, Self::Openrouter];

    /// Where its API key lives in the secret store.
    pub fn secret_key(self) -> &'static str {
        match self {
            Self::Elevenlabs => "voice:elevenlabs",
            Self::Openrouter => "voice:openrouter",
        }
    }

    pub fn base_url(self) -> &'static str {
        match self {
            Self::Elevenlabs => "https://api.elevenlabs.io",
            Self::Openrouter => "https://openrouter.ai/api/v1",
        }
    }

    pub fn default_tts_model(self) -> &'static str {
        match self {
            Self::Elevenlabs => "eleven_flash_v2_5",
            Self::Openrouter => "mistralai/voxtral-mini-tts-2603",
        }
    }

    /// `None`: `ElevenLabs` voices belong to the account, the operator picks one.
    pub fn default_voice(self) -> Option<&'static str> {
        match self {
            Self::Elevenlabs => None,
            Self::Openrouter => Some("en_paul_neutral"),
        }
    }

    pub fn default_stt_model(self) -> &'static str {
        match self {
            Self::Elevenlabs => "scribe_v3",
            Self::Openrouter => "openai/whisper-large-v3",
        }
    }
}

/// Voice settings (no secrets), stored as JSON under the `voice` setting.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VoiceSettings {
    /// `false`: voice is switched off; no speech, dictation or calls.
    #[serde(default = "on")]
    pub enabled: bool,
    /// Who speaks and listens; `null`: voice is not set up.
    #[serde(default)]
    pub provider: Option<VoiceProvider>,
    /// Text-to-speech model; `null`: the provider's default.
    #[serde(default)]
    pub tts_model: Option<String>,
    /// Voice id (`ElevenLabs`) or name (`OpenRouter`); `null`: the
    /// provider's default. Letters, digits, `.`, `_` and `-`.
    #[serde(default)]
    pub voice: Option<String>,
    /// Speech-to-text model; `null`: the provider's default.
    #[serde(default)]
    pub stt_model: Option<String>,
    /// Voices by agent id, so agents in a call sound different; the others
    /// speak with `voice`.
    #[serde(default)]
    #[schema(value_type = BTreeMap<String, String>)]
    pub agent_voices: BTreeMap<AgentId, String>,
    /// Speaking speed, 0.7-1.2 (`ElevenLabs`); `null`: normal.
    #[serde(default)]
    pub speed: Option<f32>,
    /// Language of speech and dictation, ISO 639-1 or 639-3 (`de`, `fil`);
    /// `null`: detected.
    #[serde(default)]
    pub language: Option<String>,
}

fn on() -> bool {
    true
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: None,
            tts_model: None,
            voice: None,
            stt_model: None,
            agent_voices: BTreeMap::new(),
            speed: None,
            language: None,
        }
    }
}

impl VoiceSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.speed.is_some_and(|s| !SPEEDS.contains(&s)) {
            return Err("speed must be 0.7-1.2".into());
        }
        if let Some(lang) = &self.language
            && (!(2..=3).contains(&lang.len()) || !lang.chars().all(|c| c.is_ascii_lowercase()))
        {
            return Err("language is a 2-3 letter code such as de or fil".into());
        }
        for model in [&self.tts_model, &self.stt_model].into_iter().flatten() {
            if model.len() > 200 || model.chars().any(char::is_control) {
                return Err("models must be at most 200 printable characters".into());
            }
        }
        for voice in self.voice.iter().chain(self.agent_voices.values()) {
            check_voice(voice)?;
        }
        Ok(())
    }

    /// The voice `agent` speaks with, if any is set or the provider has one.
    pub fn voice_of(&self, provider: VoiceProvider, agent: Option<AgentId>) -> Option<&str> {
        agent
            .and_then(|a| self.agent_voices.get(&a))
            .or(self.voice.as_ref())
            .map(String::as_str)
            .or(provider.default_voice())
    }
}

/// Voices end up in a URL path (`ElevenLabs`), so they stay plain.
pub fn check_voice(voice: &str) -> Result<(), String> {
    let plain = |c: char| c.is_ascii_alphanumeric() || "._-".contains(c);
    if voice.is_empty() || voice.len() > 64 || !voice.chars().all(plain) {
        return Err("a voice is 1-64 letters, digits, '.', '_' or '-'".into());
    }
    Ok(())
}

/// What one speech request says and how.
#[derive(Debug, Clone, Copy)]
pub struct Speak<'a> {
    pub model: &'a str,
    pub voice: &'a str,
    pub text: &'a str,
    pub speed: Option<f32>,
    pub language: Option<&'a str>,
}

/// A speech request to `base` (the provider's API root) for mp3 audio.
/// Speed and language are `ElevenLabs` only.
pub fn speech(
    http: &reqwest::Client,
    base: &str,
    provider: VoiceProvider,
    key: &str,
    s: Speak<'_>,
) -> reqwest::RequestBuilder {
    let base = base.trim_end_matches('/');
    let Speak {
        model, voice, text, ..
    } = s;
    match provider {
        VoiceProvider::Elevenlabs => {
            let mut body = serde_json::json!({ "text": text, "model_id": model });
            if let Some(speed) = s.speed {
                body["voice_settings"] = serde_json::json!({ "speed": speed });
            }
            // Multilingual v2 rejects an enforced language.
            if let Some(lang) = s.language.filter(|_| model != "eleven_multilingual_v2") {
                body["language_code"] = lang.into();
            }
            http.post(format!(
                "{base}/v1/text-to-speech/{voice}/stream?output_format=mp3_44100_128"
            ))
            .header("xi-api-key", key)
            .json(&body)
        }
        VoiceProvider::Openrouter => http
            .post(format!("{base}/audio/speech"))
            .bearer_auth(key)
            .json(&serde_json::json!({
                "model": model, "input": text, "voice": voice, "response_format": "mp3",
            })),
    }
    .timeout(TIMEOUT)
}

/// Which model transcribes, and in which language if fixed (`ElevenLabs` only).
#[derive(Debug, Clone, Copy)]
pub struct Listen<'a> {
    pub model: &'a str,
    pub language: Option<&'a str>,
}

/// A transcription request for a recording of `mime` type.
pub fn transcription(
    http: &reqwest::Client,
    base: &str,
    provider: VoiceProvider,
    key: &str,
    listen: Listen<'_>,
    mime: &str,
    audio: &[u8],
) -> Result<reqwest::RequestBuilder, String> {
    let Listen { model, language } = listen;
    let mime = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let ext = audio_extension(&mime).ok_or_else(|| format!("unsupported audio type {mime:?}"))?;
    let base = base.trim_end_matches('/');
    let (request, model_field) = match provider {
        VoiceProvider::Elevenlabs => (
            http.post(format!("{base}/v1/speech-to-text"))
                .header("xi-api-key", key),
            "model_id",
        ),
        VoiceProvider::Openrouter => (
            http.post(format!("{base}/audio/transcriptions"))
                .bearer_auth(key),
            "model",
        ),
    };
    let boundary = format!("ferrobot-{}", uuid::Uuid::new_v4().simple());
    let mut fields = vec![(model_field, model)];
    if let (VoiceProvider::Elevenlabs, Some(lang)) = (provider, language) {
        fields.push(("language_code", lang));
    }
    let body = multipart(&boundary, &fields, ext, &mime, audio);
    Ok(request
        .header(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .timeout(TIMEOUT))
}

/// The file extension providers expect for a recording's MIME essence
/// (`MediaRecorder` gives `audio/webm` in Chromium, `audio/mp4` in `WebKit`).
fn audio_extension(essence: &str) -> Option<&'static str> {
    Some(match essence {
        "audio/webm" => "webm",
        "audio/ogg" => "ogg",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/flac" => "flac",
        _ => return None,
    })
}

/// `multipart/form-data` with text fields and the recording as `file`. `mime` is a known
/// essence and the fields are validated settings, so neither can break the part headers.
fn multipart(
    boundary: &str,
    fields: &[(&str, &str)],
    ext: &str,
    mime: &str,
    audio: &[u8],
) -> Vec<u8> {
    let mut body = String::new();
    for (name, value) in fields {
        let _ = write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        );
    }
    let mut body = format!(
        "{body}--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech.{ext}\"\r\n\
         Content-Type: {mime}\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(audio);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// What was said. Both providers answer `{"text": ...}` (plus fields we ignore).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Transcript {
    pub text: String,
}

/// An `ElevenLabs` voice the operator can pick.
#[derive(Debug, Serialize, ToSchema)]
pub struct VoiceOption {
    pub voice_id: String,
    pub name: String,
    /// `premade`, `cloned`, `generated`, `professional`, ...
    pub category: Option<String>,
    /// Languages the voice is verified for (codes), else its `language` label.
    pub languages: Vec<String>,
    pub accent: Option<String>,
    pub gender: Option<String>,
    pub preview_url: Option<String>,
}

/// A language a model speaks.
#[derive(Debug, Serialize, ToSchema)]
pub struct LanguageOption {
    pub code: String,
    pub name: String,
}

/// A text-to-speech model of the account.
#[derive(Debug, Serialize, ToSchema)]
pub struct ModelOption {
    pub model_id: String,
    pub name: String,
    pub languages: Vec<LanguageOption>,
}

fn text(v: &serde_json::Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Voices of one `GET /v2/voices` page and the cursor of the next.
pub fn parse_voices(page: &serde_json::Value) -> (Vec<VoiceOption>, Option<String>) {
    let voices = page["voices"].as_array().into_iter().flatten();
    let voices = voices
        .filter_map(|v| {
            let mut languages: Vec<String> = v["verified_languages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| text(&l["language"]))
                .collect();
            languages.sort();
            languages.dedup();
            if languages.is_empty() {
                languages.extend(text(&v["labels"]["language"]));
            }
            Some(VoiceOption {
                voice_id: text(&v["voice_id"]).filter(|id| check_voice(id).is_ok())?,
                name: text(&v["name"])?,
                category: text(&v["category"]),
                languages,
                accent: text(&v["labels"]["accent"]),
                gender: text(&v["labels"]["gender"]),
                preview_url: text(&v["preview_url"]),
            })
        })
        .collect();
    let next = (page["has_more"] == true)
        .then(|| text(&page["next_page_token"]))
        .flatten();
    (voices, next)
}

/// The text-to-speech models in a `GET /v1/models` answer.
pub fn parse_models(models: &serde_json::Value) -> Vec<ModelOption> {
    let models = models.as_array().into_iter().flatten();
    models
        .filter(|m| m["can_do_text_to_speech"] == true)
        .filter_map(|m| {
            Some(ModelOption {
                model_id: text(&m["model_id"])?,
                name: text(&m["name"]).unwrap_or_default(),
                languages: m["languages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| {
                        Some(LanguageOption {
                            code: text(&l["language_id"])?,
                            name: text(&l["name"]).unwrap_or_default(),
                        })
                    })
                    .collect(),
            })
        })
        .collect()
}

/// How live speech is cut into messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LiveMode {
    /// The operator decides when to stop; the text goes into the message box.
    Dictation,
    /// Hands-free: a pause ends what was said.
    Call,
}

/// Sample rates `ElevenLabs` takes as raw 16-bit mono PCM.
const LIVE_RATES: [u32; 6] = [8000, 16000, 22050, 24000, 44100, 48000];

/// A request for a single-use token of the realtime transcription.
pub fn live_token(http: &reqwest::Client, base: &str, key: &str) -> reqwest::RequestBuilder {
    let base = base.trim_end_matches('/');
    http.post(format!(
        "{base}/v1/single-use-token/realtime_scribe?max_session_duration_seconds={LIVE_MAX_SECONDS}"
    ))
    .header("xi-api-key", key)
    .timeout(TIMEOUT)
}

/// The WebSocket the operator's app opens with a single-use `token`.
pub fn live_url(
    base: &str,
    token: &str,
    mode: LiveMode,
    sample_rate: u32,
    language: Option<&str>,
) -> Result<String, String> {
    if !LIVE_RATES.contains(&sample_rate) {
        return Err(format!("sample_rate must be one of {LIVE_RATES:?}"));
    }
    let mut url =
        url::Url::parse(base.trim_end_matches('/')).map_err(|e| format!("voice upstream: {e}"))?;
    let scheme = if url.scheme() == "http" { "ws" } else { "wss" };
    url.set_scheme(scheme)
        .map_err(|()| "voice upstream has no WebSocket scheme".to_owned())?;
    url.set_path("/v1/speech-to-text/realtime");
    {
        let mut q = url.query_pairs_mut();
        q.clear()
            .append_pair("model_id", LIVE_STT_MODEL)
            .append_pair("token", token)
            .append_pair("audio_format", &format!("pcm_{sample_rate}"));
        match mode {
            LiveMode::Dictation => q.append_pair("commit_strategy", "manual"),
            LiveMode::Call => q
                .append_pair("commit_strategy", "vad")
                .append_pair("vad_silence_threshold_secs", "1.2"),
        };
        if let Some(lang) = language {
            q.append_pair("language_code", lang);
        }
    }
    Ok(url.into())
}

/// What speech and transcription may still spend this minute.
#[derive(Debug, Default)]
pub struct Budget {
    window: Option<Instant>,
    chars: usize,
    transcriptions: u32,
}

impl Budget {
    /// Takes `chars` and `transcriptions` from this minute's budget; `false` when that
    /// would overrun it.
    pub fn spend(&mut self, chars: usize, transcriptions: u32) -> bool {
        let now = Instant::now();
        if self
            .window
            .is_none_or(|w| now.duration_since(w) >= Duration::from_secs(60))
        {
            *self = Self {
                window: Some(now),
                ..Self::default()
            };
        }
        if self.chars + chars > SPEECH_CHARS_PER_MINUTE
            || self.transcriptions + transcriptions > TRANSCRIPTIONS_PER_MINUTE
        {
            return false;
        }
        self.chars += chars;
        self.transcriptions += transcriptions;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(request: &reqwest::Request) -> Vec<u8> {
        request.body().unwrap().as_bytes().unwrap().to_vec()
    }

    #[test]
    fn elevenlabs_speech_streams_mp3_with_the_key_header() {
        let http = reqwest::Client::new();
        let say = |model, speed, language| Speak {
            model,
            voice: "voice123",
            text: "Hallo",
            speed,
            language,
        };
        let r = speech(
            &http,
            "https://api.elevenlabs.io/",
            VoiceProvider::Elevenlabs,
            "xi-secret",
            say("eleven_flash_v2_5", None, None),
        )
        .build()
        .unwrap();
        assert_eq!(
            r.url().as_str(),
            "https://api.elevenlabs.io/v1/text-to-speech/voice123/stream?output_format=mp3_44100_128"
        );
        assert_eq!(r.headers()["xi-api-key"], "xi-secret");
        assert!(r.headers().get("authorization").is_none());
        let json: serde_json::Value = serde_json::from_slice(&body(&r)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"text": "Hallo", "model_id": "eleven_flash_v2_5"})
        );

        // Speed and language travel along; multilingual v2 refuses a language.
        for (model, lang) in [
            ("eleven_flash_v2_5", Some("de")),
            ("eleven_multilingual_v2", None),
        ] {
            let r = speech(
                &http,
                "https://api.elevenlabs.io",
                VoiceProvider::Elevenlabs,
                "k",
                say(model, Some(1.1), Some("de")),
            )
            .build()
            .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body(&r)).unwrap();
            assert_eq!(
                json["voice_settings"]["speed"]
                    .as_f64()
                    .map(|s| (s * 10.0).round()),
                Some(11.0)
            );
            assert_eq!(json["language_code"].as_str(), lang, "{model}");
        }
    }

    #[test]
    fn openrouter_speech_asks_for_mp3() {
        let http = reqwest::Client::new();
        let r = speech(
            &http,
            VoiceProvider::Openrouter.base_url(),
            VoiceProvider::Openrouter,
            "sk-or",
            Speak {
                model: "m",
                voice: "en_paul_neutral",
                text: "Hi",
                speed: Some(1.1),
                language: Some("de"),
            },
        )
        .build()
        .unwrap();
        assert_eq!(
            r.url().as_str(),
            "https://openrouter.ai/api/v1/audio/speech"
        );
        assert_eq!(r.headers()["authorization"], "Bearer sk-or");
        let json: serde_json::Value = serde_json::from_slice(&body(&r)).unwrap();
        assert_eq!(json["response_format"], "mp3", "OpenRouter defaults to pcm");
        assert_eq!(json["voice"], "en_paul_neutral");
        assert_eq!(json["input"], "Hi");
        assert!(json.get("voice_settings").is_none() && json.get("language_code").is_none());
    }

    #[test]
    fn transcription_is_multipart_with_the_recording() {
        let http = reqwest::Client::new();
        let r = transcription(
            &http,
            "https://api.elevenlabs.io",
            VoiceProvider::Elevenlabs,
            "xi",
            Listen {
                model: "scribe_v3",
                language: Some("de"),
            },
            "audio/webm;codecs=opus",
            b"\x1a\x45\xdf\xa3audio",
        )
        .unwrap()
        .build()
        .unwrap();
        assert_eq!(r.url().path(), "/v1/speech-to-text");
        let ct = r.headers()["content-type"].to_str().unwrap().to_owned();
        let boundary = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
        let text = String::from_utf8_lossy(&body(&r)).into_owned();
        assert!(text.starts_with(&format!("--{boundary}\r\n")));
        assert!(text.contains("name=\"model_id\"\r\n\r\nscribe_v3\r\n"));
        assert!(text.contains("name=\"language_code\"\r\n\r\nde\r\n"));
        assert!(text.contains("filename=\"speech.webm\"\r\nContent-Type: audio/webm\r\n\r\n"));
        assert!(text.ends_with(&format!("audio\r\n--{boundary}--\r\n")));

        let r = transcription(
            &http,
            "https://openrouter.ai/api/v1",
            VoiceProvider::Openrouter,
            "sk-or",
            Listen {
                model: "openai/whisper-large-v3",
                language: Some("de"),
            },
            "audio/mp4",
            b"x",
        )
        .unwrap()
        .build()
        .unwrap();
        assert_eq!(r.url().path(), "/api/v1/audio/transcriptions");
        assert_eq!(r.headers()["authorization"], "Bearer sk-or");
        let text = String::from_utf8_lossy(&body(&r)).into_owned();
        assert!(text.contains("name=\"model\"\r\n\r\nopenai/whisper-large-v3\r\n"));
        assert!(text.contains("filename=\"speech.m4a\""));
        assert!(!text.contains("language_code"));

        assert!(
            transcription(
                &http,
                "x",
                VoiceProvider::Openrouter,
                "k",
                Listen {
                    model: "m",
                    language: None
                },
                "video/mp4",
                b""
            )
            .is_err()
        );
    }

    #[test]
    fn settings_check_speed_and_language() {
        let with = |speed, language: Option<&str>| VoiceSettings {
            speed,
            language: language.map(str::to_owned),
            ..VoiceSettings::default()
        };
        assert!(
            VoiceSettings::default().enabled,
            "voice is on unless switched off"
        );
        for (speed, lang, ok) in [
            (Some(1.0), Some("de"), true),
            (Some(0.7), Some("fil"), true),
            (Some(1.3), None, false),
            (None, Some("DE"), false),
            (None, Some("deutsch"), false),
        ] {
            assert_eq!(
                with(speed, lang).validate().is_ok(),
                ok,
                "{speed:?} {lang:?}"
            );
        }
        let old: VoiceSettings = serde_json::from_str(r#"{"provider":"elevenlabs"}"#).unwrap();
        assert!(old.enabled, "settings saved before the switch stay on");
    }

    #[test]
    fn live_urls_carry_the_token_never_the_key() {
        let url = live_url(
            "https://api.elevenlabs.io",
            "tok",
            LiveMode::Call,
            16000,
            Some("de"),
        )
        .unwrap();
        let u = url::Url::parse(&url).unwrap();
        assert_eq!(
            (u.scheme(), u.path()),
            ("wss", "/v1/speech-to-text/realtime")
        );
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["token"], "tok");
        assert_eq!(q["audio_format"], "pcm_16000");
        assert_eq!(q["commit_strategy"], "vad");
        assert_eq!(q["language_code"], "de");
        let dictation =
            live_url("http://127.0.0.1:9", "t", LiveMode::Dictation, 48000, None).unwrap();
        assert!(
            dictation.starts_with("ws://127.0.0.1:9/")
                && dictation.contains("commit_strategy=manual")
        );
        assert!(live_url("https://x", "t", LiveMode::Call, 11025, None).is_err());
        let r = live_token(&reqwest::Client::new(), "https://api.elevenlabs.io/", "xi")
            .build()
            .unwrap();
        assert_eq!(r.url().path(), "/v1/single-use-token/realtime_scribe");
        assert_eq!(r.headers()["xi-api-key"], "xi");
    }

    #[test]
    fn catalog_lists_tts_models_and_voices_with_languages() {
        let page = serde_json::json!({
            "voices": [
                {"voice_id": "v1", "name": "Ada", "category": "premade",
                 "labels": {"accent": "british", "gender": "female"},
                 "verified_languages": [{"language": "en"}, {"language": "de"}, {"language": "en"}],
                 "preview_url": "https://x/p.mp3"},
                {"voice_id": "v2", "name": "Ben", "labels": {"language": "fr"}},
                {"voice_id": "bad id/..", "name": "Evil"},
                {"name": "No id"}
            ],
            "has_more": true, "next_page_token": "c2"
        });
        let (voices, next) = parse_voices(&page);
        assert_eq!(next.as_deref(), Some("c2"));
        assert_eq!(
            voices.len(),
            2,
            "ids that cannot be a URL path segment are dropped"
        );
        assert_eq!(voices[0].languages, ["de", "en"]);
        assert_eq!(voices[1].languages, ["fr"]);
        let models = parse_models(&serde_json::json!([
            {"model_id": "eleven_flash_v2_5", "name": "Flash", "can_do_text_to_speech": true,
             "languages": [{"language_id": "de", "name": "German"}]},
            {"model_id": "eleven_multilingual_sts_v2", "can_do_text_to_speech": false}
        ]));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].languages[0].code, "de");
    }

    #[test]
    fn transcripts_parse_from_either_provider() {
        let t: Transcript = serde_json::from_str(
            r#"{"text":"hello","language_code":"en","words":[],"usage":{"seconds":1}}"#,
        )
        .unwrap();
        assert_eq!(t.text, "hello");
    }

    #[test]
    fn voices_stay_plain() {
        assert!(check_voice("JBFqnCBsd6RMkjVDRZzb").is_ok());
        assert!(check_voice("en_paul_neutral").is_ok());
        assert!(check_voice("../v1/history").is_err());
        assert!(check_voice("a b").is_err());
        assert!(check_voice("").is_err());
    }

    #[test]
    fn budget_caps_each_minute() {
        let mut b = Budget::default();
        assert!(b.spend(4000, 0));
        assert!(!b.spend(4000, 0), "over the minute's characters");
        assert!(b.spend(2000, 0));
        for _ in 0..TRANSCRIPTIONS_PER_MINUTE {
            assert!(b.spend(0, 1));
        }
        assert!(!b.spend(0, 1));
        b.window = Instant::now().checked_sub(Duration::from_secs(61));
        assert!(b.spend(4000, 1), "a new minute starts fresh");
    }
}
