import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";

import type { UpdateVoice, VoiceCatalog, VoiceProvider, VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import { Button, Switch } from "./ui.tsx";

const LABELS: Record<VoiceProvider, string> = { elevenlabs: "ElevenLabs", openrouter: "OpenRouter" };
const STT_MODELS = ["scribe_v3", "scribe_v2"];
/** Shown when the provider lists no languages (OpenRouter). */
const COMMON_LANGUAGES: [string, string][] = [
  ["en", "English"], ["de", "German"], ["fr", "French"], ["es", "Spanish"], ["it", "Italian"],
  ["nl", "Dutch"], ["pt", "Portuguese"], ["pl", "Polish"], ["ja", "Japanese"], ["zh", "Chinese"],
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
export function languagesOf(catalog: VoiceCatalog | null, model: string | null | undefined): [string, string][] {
  const listed = catalog?.tts_models.find((m) => m.model_id === model)?.languages ?? catalog?.tts_models[0]?.languages;
  return listed?.length ? listed.map((l) => [l.code, l.name || l.code]) : COMMON_LANGUAGES;
}

/** A pick from `options`, or anything typed after choosing "Custom…". */
export function Choice(props: {
  value: string;
  options: [string, string][];
  placeholder: string;
  onChange: (v: string) => void;
}): JSX.Element {
  const known = props.value === "" || props.options.some(([v]) => v === props.value);
  const [custom, setCustom] = useState(!known);
  if (custom) {
    return (
      <span class="choice">
        <input value={props.value} spellcheck={false} placeholder={props.placeholder} onInput={(e) => { props.onChange(e.currentTarget.value); }} />
        <Button small variant="ghost" onClick={() => { setCustom(false); props.onChange(""); }}>List</Button>
      </span>
    );
  }
  return (
    <select
      value={props.value}
      onChange={(e) => {
        const v = e.currentTarget.value;
        if (v === "\u0000custom") setCustom(true);
        else props.onChange(v);
      }}
    >
      <option value="">{props.placeholder}</option>
      {props.options.map(([v, label]) => <option key={v} value={v}>{label}</option>)}
      <option value={"\u0000custom"}>Custom…</option>
    </select>
  );
}

/** Speech provider, key, voice, models, language and speed. The key goes to the daemon’s keychain and never comes back. */
export function VoiceSettings(): JSX.Element {
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
  const [key, setKey] = useState("");
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
    client.voice().then((v) => { show(v, v.provider ?? "elevenlabs"); }, (err: unknown) => {
      toast(err instanceof Error ? err.message : String(err));
    });
  }, []);

  // The account's voices and models, once the provider with a key is saved and voice is on.
  const ready = view?.provider === "elevenlabs" && provider === "elevenlabs" && info?.has_key && view.enabled !== false;
  useEffect(() => {
    if (!ready) return;
    client.voiceCatalog().then(setCatalog, (err: unknown) => {
      toast(`Could not load your voices: ${err instanceof Error ? err.message : String(err)}`);
    });
  }, [ready]);

  const save = async (apiKey?: string | null): Promise<void> => {
    if (!view) return;
    setBusy(true);
    try {
      const v = await client.updateVoice({
        ...voiceUpdate(view, {
          enabled,
          provider,
          voice: voice.trim() || null,
          agent_voices: Object.fromEntries(Object.entries(agentVoices).filter(([, id]) => id.trim())),
          tts_model: ttsModel.trim() || null,
          stt_model: sttModel.trim() || null,
          language: language || null,
          speed: speed === 1 ? null : speed,
        }),
        ...(apiKey !== undefined && { api_key: apiKey }),
      });
      show(v, provider);
      setKey("");
      toast("Voice saved", "info");
    } catch (err) {
      toast(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const voices: [string, string][] =
    catalog?.voices.map((v) => [v.voice_id, `${v.name}${v.languages.length ? ` · ${v.languages.join(", ")}` : ""}`]) ?? [];
  const models: [string, string][] = catalog?.tts_models.map((m) => [m.model_id, m.name || m.model_id]) ?? [];

  return (
    <div>
      <section class="settings-section form">
        <span class="field">
          <span class="field-row">
            Voice in chats
            <Switch checked={enabled} label="Voice in chats" onChange={setEnabled} />
          </span>
          <span class="hint">Off hides the call and microphone buttons; settings and keys are kept.</span>
        </span>
      </section>
      <section class="settings-section form">
        <label>
          Provider
          <select value={provider} onChange={(e) => { if (view) show(view, e.currentTarget.value as VoiceProvider); }}>
            {(Object.keys(LABELS) as VoiceProvider[]).map((k) => (
              <option key={k} value={k}>{LABELS[k]}</option>
            ))}
          </select>
        </label>
        <label>
          API key
          <input
            type="password"
            autoComplete="off"
            value={key}
            placeholder={info?.has_key ? "Stored. Type to replace it" : ""}
            onInput={(e) => { setKey(e.currentTarget.value); }}
          />
          <span class="hint">Stored in the keychain of the daemon’s computer. Agents never see it.</span>
        </label>
        {info?.has_key && (
          <div>
            <Button small variant="ghost" disabled={busy} onClick={() => void save(null)}>Remove key</Button>
          </div>
        )}
      </section>
      <section class="settings-section form">
        <label>
          Voice
          <Choice value={voice} options={voices} placeholder={info?.default_voice ?? (ready ? "Pick a voice" : "Default")} onChange={setVoice} />
          {provider === "elevenlabs" && !ready && <span class="hint">Save your key to pick from your ElevenLabs voices.</span>}
        </label>
        <label>
          Speech model
          <Choice value={ttsModel} options={models} placeholder={info?.default_tts_model ?? "Default"} onChange={setTtsModel} />
        </label>
        <label>
          Transcription model
          <Choice
            value={sttModel}
            options={provider === "elevenlabs" ? STT_MODELS.map((m) => [m, m]) : []}
            placeholder={info?.default_stt_model ?? "Default"}
            onChange={setSttModel}
          />
        </label>
        <label>
          Language
          <select value={language} onChange={(e) => { setLanguage(e.currentTarget.value); }}>
            <option value="">Detect automatically</option>
            {languagesOf(catalog, ttsModel || info?.default_tts_model).map(([code, name]) => (
              <option key={code} value={code}>{name}</option>
            ))}
          </select>
        </label>
        {provider === "elevenlabs" && (
          <label>
            Speed · {speed.toFixed(2)}×
            <input type="range" min={0.7} max={1.2} step={0.05} value={speed} onInput={(e) => { setSpeed(Number(e.currentTarget.value)); }} />
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
                options={voices}
                placeholder="Same as above"
                onChange={(v) => { setAgentVoices({ ...agentVoices, [a.id]: v }); }}
              />
            </label>
          ))}
        </section>
      )}
      <div class="settings-actions">
        <Button variant="primary" disabled={busy || !view} onClick={() => void save(key.trim() || undefined)}>
          Save
        </Button>
      </div>
    </div>
  );
}
