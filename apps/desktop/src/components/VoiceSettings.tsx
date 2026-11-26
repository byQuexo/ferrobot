import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { Trash2 } from "lucide-preact";

import type { DaemonClient, UpdateVoice, VoiceCatalog, VoiceProvider, VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import { speedLabel, volumeLabel } from "../lib/call.ts";
import { errorText } from "../lib/format.ts";
import { withSaved } from "../lib/models.ts";
import { hasVoiceKey, pickProvider, switchProvider } from "../lib/voice.ts";
import { PrefRow } from "./AppearanceSettings.tsx";
import { LinkOut } from "./ProvidersSettings.tsx";
import { Button, Choice, IconButton, Skeleton, Slider, Switch } from "./ui.tsx";

export const VOICE_LABELS: Record<VoiceProvider, string> = {
  elevenlabs: "ElevenLabs",
  openrouter: "OpenRouter",
};
const PROVIDER_HINTS: Record<VoiceProvider, string> = {
  elevenlabs: "Speech, hands-free calls, your account’s voices and a speed setting.",
  openrouter: "Speech and transcription. In calls you dictate with the microphone.",
};
const KEY_LINKS: Record<VoiceProvider, string> = {
  elevenlabs: "https://elevenlabs.io/app/settings/api-keys",
  openrouter: "https://openrouter.ai/settings/keys",
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

/** Applies `changes` to the daemon's current voice settings; the update replaces all of them. */
export async function patchVoice(
  client: DaemonClient,
  changes: Partial<UpdateVoice>,
): Promise<VoiceView> {
  const view = await client.voice();
  return client.updateVoice({
    enabled: view.enabled,
    provider: view.provider ?? null,
    tts_model: view.tts_model ?? null,
    voice: view.voice ?? null,
    stt_model: view.stt_model ?? null,
    agent_voices: view.agent_voices ?? {},
    speed: view.speed ?? null,
    language: view.language ?? null,
    ...changes,
  });
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

/** The account's voices as picker options, each with the languages it speaks. */
export const voiceOptions = (catalog: VoiceCatalog | null): [string, string][] =>
  catalog?.voices.map((v) => [
    v.voice_id,
    `${v.name}${v.languages.length ? ` · ${v.languages.join(", ")}` : ""}`,
  ]) ?? [];

/** The speech provider's key: pasted here, kept in the daemon's keychain, never shown again. */
function KeyRow(props: {
  kind: VoiceProvider;
  stored: boolean;
  /** Why the voices did not load, which mostly means a wrong key. */
  error: string;
  onSave: (key: string | null) => Promise<boolean>;
}): JSX.Element {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const label = VOICE_LABELS[props.kind];
  useEffect(() => {
    if (editing) input.current?.focus();
  }, [editing]);
  const submit = async (key: string | null): Promise<void> => {
    setBusy(true);
    const saved = await props.onSave(key);
    setBusy(false);
    if (saved) {
      setValue("");
      setEditing(false);
    }
  };
  return (
    <PrefRow
      title="API key"
      hint={
        <>
          {props.stored ? (
            "In the keychain of the daemon’s computer. Agents never see it."
          ) : (
            <>
              Paste your {label} key. It goes to the daemon’s keychain and is never shown again.{" "}
              <LinkOut label="Get a key" url={KEY_LINKS[props.kind]} />
            </>
          )}
          {props.error && (
            <span class="hint bad" role="alert">
              {" "}
              Could not load your voices: {props.error}
            </span>
          )}
        </>
      }
    >
      {editing || !props.stored ? (
        <form
          class="voice-key"
          onSubmit={(e) => {
            e.preventDefault();
            void submit(value.trim());
          }}
        >
          <input
            ref={input}
            type="password"
            autoComplete="off"
            spellcheck={false}
            aria-label={`${label} API key`}
            placeholder="Paste your key"
            value={value}
            onInput={(e) => {
              setValue(e.currentTarget.value);
            }}
          />
          <Button small variant="primary" type="submit" disabled={busy || !value.trim()}>
            Save
          </Button>
          {props.stored && (
            <Button
              small
              variant="ghost"
              onClick={() => {
                setEditing(false);
                setValue("");
              }}
            >
              Cancel
            </Button>
          )}
        </form>
      ) : (
        <span class="voice-key">
          <span class="voice-key-state">
            <span class="dot p-awake" aria-hidden="true" />
            Stored
          </span>
          <Button
            small
            variant="ghost"
            onClick={() => {
              setEditing(true);
            }}
          >
            Replace
          </Button>
          <IconButton
            label={`Remove the ${label} key`}
            tone="danger"
            disabled={busy}
            onClick={() => void submit(null)}
          >
            <Trash2 size={16} />
          </IconButton>
        </span>
      )}
    </PrefRow>
  );
}

/** Speech provider and its key, voices, language, speed and calls; every change is saved at once. */
export function VoiceSettings(): JSX.Element {
  const { client, toast, agents, call } = useApp();
  /** What the page shows: the daemon's settings with the changes still being saved. */
  const [view, setView] = useState<VoiceView | null>(null);
  /** The daemon's last answer; the voice list needs the provider and key saved there. */
  const [saved, setSaved] = useState<VoiceView | null>(null);
  const [catalog, setCatalog] = useState<VoiceCatalog | null>(null);
  const [catalogError, setCatalogError] = useState("");
  /** A provider chosen but not saved yet: it waits for its key. */
  const [picked, setPicked] = useState<VoiceProvider | null>(null);
  const [confirming, setConfirming] = useState<VoiceProvider | null>(null);
  const [keySaves, setKeySaves] = useState(0);
  const [volume, setVolume] = useState(call.volume);
  const [pushToTalk, setPushToTalk] = useState(call.pushToTalk);
  const saving = useRef(Promise.resolve());

  const show = (v: VoiceView): void => {
    setView(v);
    setSaved(v);
  };
  useEffect(() => {
    client.voice().then(show, (err: unknown) => {
      toast(errorText(err));
    });
  }, []);
  // The call bar changes these too.
  useEffect(
    () =>
      call.subscribe(() => {
        setVolume(call.volume);
        setPushToTalk(call.pushToTalk);
      }),
    [call],
  );

  const ready = saved?.provider === "elevenlabs" && saved.enabled !== false && hasVoiceKey(saved);
  useEffect(() => {
    setCatalogError("");
    if (!ready) {
      setCatalog(null);
      return;
    }
    client.voiceCatalog().then(setCatalog, (err: unknown) => {
      setCatalog(null);
      setCatalogError(errorText(err));
    });
  }, [ready, keySaves]);

  if (!view) return <Skeleton lines={4} />;

  // One after another, each on fresh settings, so the call bar's speed is not undone.
  const save = (changes: Partial<UpdateVoice>): Promise<void> => {
    setView((v) => v && { ...v, ...changes });
    saving.current = saving.current.then(async () => {
      try {
        setSaved(await patchVoice(client, changes));
      } catch (err) {
        toast(errorText(err));
        await client.voice().then(show, () => undefined);
      }
    });
    return saving.current;
  };

  const shown = picked ?? view.provider ?? null;
  const info = view.providers.find((p) => p.kind === shown);
  const choose = (kind: VoiceProvider, confirmed = false): void => {
    const step = pickProvider(view, shown, kind, confirmed);
    setConfirming(step === "confirm" ? kind : null);
    if (step === "back") setPicked(null);
    else if (step === "key") setPicked(kind);
    else if (step === "use") {
      setPicked(null);
      void save(switchProvider(kind));
    }
  };
  const saveKey = async (kind: VoiceProvider, key: string | null): Promise<boolean> => {
    try {
      const v = await client.updateVoiceKey(kind, key);
      setView((cur) => cur && { ...cur, providers: v.providers });
      setSaved(v);
      if (key && kind !== view.provider) {
        setPicked(null);
        await save(switchProvider(kind));
      } else setKeySaves((n) => n + 1);
      toast(key ? "Key saved" : "Key removed", "info");
      return true;
    } catch (err) {
      toast(errorText(err));
      return false;
    }
  };

  const elevenlabs = view.provider === "elevenlabs";
  const voices = voiceOptions(catalog);
  const models: [string, string][] =
    catalog?.tts_models.map((m) => [m.model_id, m.name || m.model_id]) ?? [];
  const voiceHint = !elevenlabs
    ? undefined
    : view.enabled === false
      ? "Switch voice on to load your voices."
      : catalogError
        ? "Your voices could not be loaded; check the key."
        : !ready
          ? "Save your key to load your voices."
          : catalog
            ? "From your ElevenLabs account."
            : "Loading your voices…";
  const voiceOf = (agent: string): string => view.agent_voices?.[agent] ?? "";

  return (
    <div class="voice-settings">
      <h3 class="pref-head">Provider and key</h3>
      <ul class="pref-list">
        <PrefRow
          title="Voice in chats"
          hint="Off hides the call and microphone buttons; settings and keys are kept."
        >
          <Switch
            checked={view.enabled !== false}
            label="Voice in chats"
            onChange={(enabled) => void save({ enabled })}
          />
        </PrefRow>
        <PrefRow
          title="Provider"
          hint={shown ? PROVIDER_HINTS[shown] : "Pick who speaks and listens."}
        >
          <div class="segmented" role="radiogroup" aria-label="Provider">
            {(Object.keys(VOICE_LABELS) as VoiceProvider[]).map((k) => (
              <button
                key={k}
                type="button"
                role="radio"
                aria-checked={shown === k}
                onClick={() => {
                  choose(k);
                }}
              >
                {VOICE_LABELS[k]}
              </button>
            ))}
          </div>
          {confirming && (
            <span class="pref-confirm" role="alert">
              Switching clears the voices and models you picked.
              <Button
                small
                variant="primary"
                onClick={() => {
                  choose(confirming, true);
                }}
              >
                Switch to {VOICE_LABELS[confirming]}
              </Button>
              <Button
                small
                variant="ghost"
                onClick={() => {
                  setConfirming(null);
                }}
              >
                Cancel
              </Button>
            </span>
          )}
        </PrefRow>
        {shown && (
          <KeyRow
            key={shown}
            kind={shown}
            stored={info?.has_key === true}
            error={shown === "elevenlabs" && shown === saved?.provider ? catalogError : ""}
            onSave={(key) => saveKey(shown, key)}
          />
        )}
        {view.provider && !picked && (
          <>
            <PrefRow title="Speech model" hint="Turns replies into speech.">
              <Choice
                label="Speech model"
                lazy
                value={view.tts_model ?? ""}
                options={withSaved(models, view.tts_model ?? "")}
                placeholder={info?.default_tts_model ?? "Default"}
                onChange={(v) => void save({ tts_model: v.trim() || null })}
              />
            </PrefRow>
            <PrefRow title="Transcription model" hint="Turns your speech into text.">
              <Choice
                label="Transcription model"
                lazy
                value={view.stt_model ?? ""}
                options={
                  elevenlabs
                    ? withSaved(
                        STT_MODELS.map((m) => [m, m]),
                        view.stt_model ?? "",
                      )
                    : []
                }
                placeholder={info?.default_stt_model ?? "Default"}
                onChange={(v) => void save({ stt_model: v.trim() || null })}
              />
            </PrefRow>
          </>
        )}
      </ul>
      {view.provider && !picked && (
        <>
          <h3 class="pref-head">Voices</h3>
          <p class="pref-sub">Give agents their own voice, so they sound different in a call.</p>
          <ul class="pref-list">
            <PrefRow title="Default voice" hint={voiceHint}>
              <Choice
                label="Default voice"
                lazy
                value={view.voice ?? ""}
                options={withSaved(voices, view.voice ?? "")}
                placeholder={info?.default_voice ?? (ready ? "Pick a voice" : "Default")}
                onChange={(v) => void save({ voice: v.trim() || null })}
              />
            </PrefRow>
            {[...agents.values()].map((a) => (
              <PrefRow key={a.id} title={a.name}>
                <Choice
                  label={`Voice of ${a.name}`}
                  lazy
                  value={voiceOf(a.id)}
                  options={withSaved(voices, voiceOf(a.id))}
                  placeholder="Default"
                  onChange={(v) => {
                    const rest = Object.fromEntries(
                      Object.entries(view.agent_voices ?? {}).filter(([id]) => id !== a.id),
                    );
                    void save({ agent_voices: v.trim() ? { ...rest, [a.id]: v.trim() } : rest });
                  }}
                />
              </PrefRow>
            ))}
          </ul>

          <h3 class="pref-head">Language and speed</h3>
          <ul class="pref-list">
            <PrefRow title="Language" hint="For speech and dictation.">
              <select
                aria-label="Language"
                value={view.language ?? ""}
                onChange={(e) => void save({ language: e.currentTarget.value || null })}
              >
                <option value="">Detect automatically</option>
                {withSaved(
                  languagesOf(catalog, view.tts_model || info?.default_tts_model),
                  view.language ?? "",
                ).map(([code, name]) => (
                  <option key={code} value={code}>
                    {name}
                  </option>
                ))}
              </select>
            </PrefRow>
            {elevenlabs && (
              <PrefRow title="Speed" hint="How fast agents speak.">
                <span class="pref-slider">
                  <Slider
                    label="Speed"
                    text={speedLabel(view.speed ?? null)}
                    min={0.7}
                    max={1.2}
                    step={0.05}
                    value={view.speed ?? 1}
                    onInput={(speed) => {
                      setView((v) => v && { ...v, speed });
                    }}
                    onChange={(speed) => void save({ speed: speed === 1 ? null : speed })}
                  />
                  <output aria-hidden="true">{speedLabel(view.speed ?? null)}</output>
                </span>
              </PrefRow>
            )}
          </ul>

          <h3 class="pref-head">Calls</h3>
          <ul class="pref-list">
            {elevenlabs && (
              <PrefRow
                title="Push to talk"
                hint="Talk only while you hold Space or the talk button. Saved on this computer."
              >
                <Switch
                  checked={pushToTalk}
                  label="Push to talk"
                  onChange={(on) => {
                    call.setPushToTalk(on);
                  }}
                />
              </PrefRow>
            )}
            <PrefRow
              title="Call volume"
              hint="How loud agents speak in a call. Saved on this computer."
            >
              <span class="pref-slider">
                <Slider
                  label="Call volume"
                  text={volumeLabel(volume)}
                  min={0}
                  max={1}
                  step={0.05}
                  value={volume}
                  onInput={(v) => {
                    call.setVolume(v);
                  }}
                />
                <output aria-hidden="true">{volumeLabel(volume)}</output>
              </span>
            </PrefRow>
          </ul>
        </>
      )}
    </div>
  );
}
