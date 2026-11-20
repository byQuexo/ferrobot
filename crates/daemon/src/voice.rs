//! Speech for the operator's chat: text to speech and speech to text with
//! `ElevenLabs` or `OpenRouter`. The daemon holds the key (secret store,
//! `voice:<provider>`); neither the key, the text nor the audio is logged.

use std::time::{Duration, Instant};

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
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct VoiceSettings {
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
}

impl VoiceSettings {
    pub fn validate(&self) -> Result<(), String> {
        for model in [&self.tts_model, &self.stt_model].into_iter().flatten() {
            if model.len() > 200 || model.chars().any(char::is_control) {
                return Err("models must be at most 200 printable characters".into());
            }
        }
        if let Some(voice) = &self.voice {
            check_voice(voice)?;
        }
        Ok(())
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

/// A speech request to `base` (the provider's API root) for mp3 audio.
pub fn speech(
    http: &reqwest::Client,
    base: &str,
    provider: VoiceProvider,
    key: &str,
    model: &str,
    voice: &str,
    text: &str,
) -> reqwest::RequestBuilder {
    let base = base.trim_end_matches('/');
    match provider {
        VoiceProvider::Elevenlabs => http
            .post(format!(
                "{base}/v1/text-to-speech/{voice}/stream?output_format=mp3_44100_128"
            ))
            .header("xi-api-key", key)
            .json(&serde_json::json!({ "text": text, "model_id": model })),
        VoiceProvider::Openrouter => http
            .post(format!("{base}/audio/speech"))
            .bearer_auth(key)
            .json(&serde_json::json!({
                "model": model, "input": text, "voice": voice, "response_format": "mp3",
            })),
    }
    .timeout(TIMEOUT)
}

/// A transcription request for a recording of `mime` type.
pub fn transcription(
    http: &reqwest::Client,
    base: &str,
    provider: VoiceProvider,
    key: &str,
    model: &str,
    mime: &str,
    audio: &[u8],
) -> Result<reqwest::RequestBuilder, String> {
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
    let body = multipart(&boundary, model_field, model, ext, &mime, audio);
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

/// `multipart/form-data` with the model field and the recording as `file`
/// (`mime` is a known essence, so it cannot break the part headers).
fn multipart(
    boundary: &str,
    model_field: &str,
    model: &str,
    ext: &str,
    mime: &str,
    audio: &[u8],
) -> Vec<u8> {
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"{model_field}\"\r\n\r\n{model}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech.{ext}\"\r\n\
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

/// What speech and transcription may still spend this minute.
#[derive(Debug, Default)]
pub struct Budget {
    window: Option<Instant>,
    chars: usize,
    transcriptions: u32,
}

impl Budget {
    /// Takes `chars` characters of speech or one transcription from this
    /// minute's budget; `false` when it is used up.
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
        let r = speech(
            &http,
            "https://api.elevenlabs.io/",
            VoiceProvider::Elevenlabs,
            "xi-secret",
            "eleven_flash_v2_5",
            "voice123",
            "Hallo",
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
    }

    #[test]
    fn openrouter_speech_asks_for_mp3() {
        let http = reqwest::Client::new();
        let r = speech(
            &http,
            VoiceProvider::Openrouter.base_url(),
            VoiceProvider::Openrouter,
            "sk-or",
            "m",
            "en_paul_neutral",
            "Hi",
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
    }

    #[test]
    fn transcription_is_multipart_with_the_recording() {
        let http = reqwest::Client::new();
        let r = transcription(
            &http,
            "https://api.elevenlabs.io",
            VoiceProvider::Elevenlabs,
            "xi",
            "scribe_v3",
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
        assert!(text.contains("filename=\"speech.webm\"\r\nContent-Type: audio/webm\r\n\r\n"));
        assert!(text.ends_with(&format!("audio\r\n--{boundary}--\r\n")));

        let r = transcription(
            &http,
            "https://openrouter.ai/api/v1",
            VoiceProvider::Openrouter,
            "sk-or",
            "openai/whisper-large-v3",
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

        assert!(
            transcription(
                &http,
                "x",
                VoiceProvider::Openrouter,
                "k",
                "m",
                "video/mp4",
                b""
            )
            .is_err()
        );
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
