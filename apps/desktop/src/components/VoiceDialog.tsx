import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";

import type { VoiceProvider, VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import { Button, Dialog } from "./ui.tsx";

const LABELS: Record<VoiceProvider, string> = { elevenlabs: "ElevenLabs", openrouter: "OpenRouter" };

/** Speech provider, key, voice and models. The key goes to the daemon’s keychain and never comes back. */
export function VoiceDialog(props: { onClose: () => void }): JSX.Element {
  const { client, toast, agents } = useApp();
  const [view, setView] = useState<VoiceView | null>(null);
  const [provider, setProvider] = useState<VoiceProvider>("elevenlabs");
  const [voice, setVoice] = useState("");
  const [agentVoices, setAgentVoices] = useState<Record<string, string>>({});
  const [ttsModel, setTtsModel] = useState("");
  const [sttModel, setSttModel] = useState("");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const info = view?.providers.find((p) => p.kind === provider);

  // Voices and models belong to one provider, so another provider starts empty.
  const show = (v: VoiceView, kind: VoiceProvider): void => {
    const saved = v.provider === kind;
    setView(v);
    setProvider(kind);
    setVoice(saved ? (v.voice ?? "") : "");
    setAgentVoices(saved ? (v.agent_voices ?? {}) : {});
    setTtsModel(saved ? (v.tts_model ?? "") : "");
    setSttModel(saved ? (v.stt_model ?? "") : "");
  };

  useEffect(() => {
    client.voice().then((v) => { show(v, v.provider ?? "elevenlabs"); }, (err: unknown) => {
      toast(err instanceof Error ? err.message : String(err));
    });
  }, []);

  const save = async (apiKey?: string | null): Promise<void> => {
    setBusy(true);
    try {
      const v = await client.updateVoice({
        provider,
        voice: voice.trim() || null,
        agent_voices: agentVoices,
        tts_model: ttsModel.trim() || null,
        stt_model: sttModel.trim() || null,
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

  return (
    <Dialog
      title="Voice"
      onClose={props.onClose}
      footer={
        <>
          <Button onClick={props.onClose}>Done</Button>
          <Button variant="primary" disabled={busy || !view} onClick={() => void save(key.trim() || undefined)}>
            Save
          </Button>
        </>
      }
    >
      <div class="form">
        <p class="muted small">
          Agents’ replies are read aloud and you can speak your messages. Switch it on with the speaker button in a chat.
        </p>
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
        <label>
          Voice
          <input
            value={voice}
            placeholder={info?.default_voice ?? "Voice id from your ElevenLabs voices"}
            onInput={(e) => { setVoice(e.currentTarget.value); }}
          />
        </label>
        <label>
          Speech model
          <input value={ttsModel} placeholder={info?.default_tts_model} onInput={(e) => { setTtsModel(e.currentTarget.value); }} />
        </label>
        <label>
          Transcription model
          <input value={sttModel} placeholder={info?.default_stt_model} onInput={(e) => { setSttModel(e.currentTarget.value); }} />
        </label>
        {agents.size > 0 && (
          <>
            <h3 class="form-head">Voice per agent</h3>
            <p class="muted small">So agents in a call sound different. Empty: the voice above.</p>
            {[...agents.values()].map((a) => (
              <label key={a.id}>
                {a.name}
                <input
                  value={agentVoices[a.id] ?? ""}
                  placeholder={voice.trim() || info?.default_voice || ""}
                  onInput={(e) => { setAgentVoices({ ...agentVoices, [a.id]: e.currentTarget.value }); }}
                />
              </label>
            ))}
          </>
        )}
      </div>
    </Dialog>
  );
}
