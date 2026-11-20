import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";

import type { UpdateVoice, VoiceCatalog, VoiceProvider, VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import { withSaved } from "../lib/models.ts";
import { Button, Choice, Switch } from "./ui.tsx";

export const VOICE_LABELS: Record<VoiceProvider, string> = {
  elevenlabs: "ElevenLabs",
  openrouter: "OpenRouter",
};
const STT_MODELS = ["scribe_v3", "scribe_v2"];
/** Shown when the provider lists no languages (OpenRouter). */
const COMMON_LANGUAGES: [string, string][] = [
  ["en", "English"],
  ["de", "German"],
  ["fr", "French"],
  ["es", "Spanish"],
  ["it", "Italian"],
  ["nl", "Dutch"],
  ["pt", "Portuguese"],
  ["pl", "Polish"],
  ["ja", "Japanese"],
  ["zh", "Chinese"],
];

/** The fields of `PUT /v1/voice` from what the daemon reported, with `changes` applied. */
export function voiceUpdate(view: VoiceView, changes: Partial<UpdateVoice>): UpdateVoice {
  return {
    enabled: view.enabled,
    provider: view.provider ?? null,
    tts_model: view.tts_model ?? null,
    voice: view.voice ?? null,
    stt_model: view.stt_model ?? null,
    agent_voices: view.agent_voices ?? {},
    speed: view.speed ?? null,
    language: view.language ?? null,
    ...changes,
  };
}

/** Languages the chosen speech model speaks, else a common list. */
export function languagesOf(
  catalog: VoiceCatalog | null,
  model: string | null | undefined,
): [string, string][] {
  const listed =
    catalog?.tts_models.find((m) => m.model_id === model)?.languages ??
    catalog?.tts_models[0]?.languages;
  return listed?.length ? listed.map((l) => [l.code, l.name || l.code]) : COMMON_LANGUAGES;
}

/** Speech provider, voice, models, language and speed; the provider's key lives in Vault. */
export function VoiceSettings(props: { onVault: () => void }): JSX.Element {
  const { client, toast, agents } = useApp();
  const [view, setView] = useState<VoiceView | null>(null);
  const [catalog, setCatalog] = useState<VoiceCatalog | null>(null);
  const [provider, setProvider] = useState<VoiceProvider>("elevenlabs");
  const [enabled, setEnabled] = useState(true);
  const [voice, setVoice] = useState("");
  const [agentVoices, setAgentVoices] = useState<Record<string, string>>({});
  const [ttsModel, setTtsModel] = useState("");
  const [sttModel, setSttModel] = useState("");
  const [language, setLanguage] = useState("");
  const [speed, setSpeed] = useState(1);
  const [busy, setBusy] = useState(false);
  const info = view?.providers.find((p) => p.kind === provider);

  // Voices and models belong to one provider, so another provider starts empty.
  const show = (v: VoiceView, kind: VoiceProvider): void => {
    const saved = v.provider === kind;
    setView(v);
    setProvider(kind);
    setEnabled(v.enabled !== false);
    setVoice(saved ? (v.voice ?? "") : "");
    setAgentVoices(saved ? (v.agent_voices ?? {}) : {});
    setTtsModel(saved ? (v.tts_model ?? "") : "");
    setSttModel(saved ? (v.stt_model ?? "") : "");
    setLanguage(v.language ?? "");
    setSpeed(v.speed ?? 1);
  };

  useEffect(() => {
    client.voice().then(
      (v) => {
        show(v, v.provider ?? "elevenlabs");
      },
      (err: unknown) => {
        toast(err instanceof Error ? err.message : String(err));
      },
    );
  }, []);

  // The account's voices and models, once the provider with a key is saved and voice is on.
  const ready =
    view?.provider === "elevenlabs" &&
    provider === "elevenlabs" &&
    info?.has_key &&
    view.enabled !== false;
  useEffect(() => {
    if (!ready) return;
    client.voiceCatalog().then(setCatalog, (err: unknown) => {
      toast(`Could not load your voices: ${err instanceof Error ? err.message : String(err)}`);
    });
  }, [ready]);

  const save = async (): Promise<void> => {
    if (!view) return;
    setBusy(true);
    try {
      const v = await client.updateVoice(
        voiceUpdate(view, {
          enabled,
          provider,
          voice: voice.trim() || null,
          agent_voices: Object.fromEntries(
            Object.entries(agentVoices).filter(([, id]) => id.trim()),
          ),
          tts_model: ttsModel.trim() || null,
          stt_model: sttModel.trim() || null,
          language: language || null,
          speed: speed === 1 ? null : speed,
        }),
      );
      show(v, provider);
      toast("Voice saved", "info");
    } catch (err) {
      toast(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const voices: [string, string][] =
    catalog?.voices.map((v) => [
      v.voice_id,
      `${v.name}${v.languages.length ? ` · ${v.languages.join(", ")}` : ""}`,
    ]) ?? [];
  const models: [string, string][] =
    catalog?.tts_models.map((m) => [m.model_id, m.name || m.model_id]) ?? [];

  return (
    <div>
      <section class="settings-section form">
        <span class="field">
          <span class="field-row">
            Voice in chats
            <Switch checked={enabled} label="Voice in chats" onChange={setEnabled} />
          </span>
          <span class="hint">
            Off hides the call and microphone buttons; settings and keys are kept.
          </span>
        </span>
      </section>
      <section class="settings-section form">
        <label>
          Provider
          <select
            value={provider}
            onChange={(e) => {
              if (view) show(view, e.currentTarget.value as VoiceProvider);
            }}
          >
            {(Object.keys(VOICE_LABELS) as VoiceProvider[]).map((k) => (
              <option key={k} value={k}>
                {VOICE_LABELS[k]}
              </option>
            ))}
          </select>
        </label>
        <span class="hint">
          {info?.has_key ? "Its key is in " : "Add its key in "}
          <button type="button" class="text-link" onClick={props.onVault}>
            Vault
          </button>
          .
        </span>
      </section>
      <section class="settings-section form">
        <label>
          Voice
          <Choice
            value={voice}
            options={withSaved(voices, voice)}
            placeholder={info?.default_voice ?? (ready ? "Pick a voice" : "Default")}
            onChange={setVoice}
          />
          {provider === "elevenlabs" && !ready && (
            <span class="hint">Save your key to pick from your ElevenLabs voices.</span>
          )}
        </label>
        <label>
          Speech model
          <Choice
            value={ttsModel}
            options={withSaved(models, ttsModel)}
            placeholder={info?.default_tts_model ?? "Default"}
            onChange={setTtsModel}
          />
        </label>
        <label>
          Transcription model
          <Choice
            value={sttModel}
            options={
              provider === "elevenlabs"
                ? withSaved(
                    STT_MODELS.map((m) => [m, m]),
                    sttModel,
                  )
                : []
            }
            placeholder={info?.default_stt_model ?? "Default"}
            onChange={setSttModel}
          />
        </label>
        <label>
          Language
          <select
            value={language}
            onChange={(e) => {
              setLanguage(e.currentTarget.value);
            }}
          >
            <option value="">Detect automatically</option>
            {languagesOf(catalog, ttsModel || info?.default_tts_model).map(([code, name]) => (
              <option key={code} value={code}>
                {name}
              </option>
            ))}
          </select>
        </label>
        {provider === "elevenlabs" && (
          <label>
            Speed · {speed.toFixed(2)}×
            <input
              type="range"
              min={0.7}
              max={1.2}
              step={0.05}
              value={speed}
              onInput={(e) => {
                setSpeed(Number(e.currentTarget.value));
              }}
            />
          </label>
        )}
      </section>
      {agents.size > 0 && (
        <section class="settings-section form">
          <h3 class="form-head">Voice per agent</h3>
          <p class="muted small">So agents in a call sound different. Empty: the voice above.</p>
          {[...agents.values()].map((a) => (
            <label key={a.id}>
              {a.name}
              <Choice
                value={agentVoices[a.id] ?? ""}
                options={withSaved(voices, agentVoices[a.id] ?? "")}
                placeholder="Same as above"
                onChange={(v) => {
                  setAgentVoices({ ...agentVoices, [a.id]: v });
                }}
              />
            </label>
          ))}
        </section>
      )}
      <div class="settings-actions">
        <Button variant="primary" disabled={busy || !view} onClick={() => void save()}>
          Save
        </Button>
      </div>
    </div>
  );
}
