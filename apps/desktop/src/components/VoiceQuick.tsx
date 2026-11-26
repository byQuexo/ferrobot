import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { Play, SlidersHorizontal } from "lucide-preact";

import type { Agent, Provider, UpdateVoice, VoiceCatalog, VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import { speedLabel } from "../lib/call.ts";
import { withSaved } from "../lib/models.ts";
import { ModelPicker } from "./ModelPicker.tsx";
import { IconButton, Slider, useDismiss } from "./ui.tsx";
import { languagesOf, patchVoice } from "./VoiceSettings.tsx";

/** The chat header's sliders button and its popover: this agent's voice, language and speed,
 * saved right away. */
export function VoiceQuick(props: {
  agent: Agent;
  view: VoiceView | null;
  onView: (v: VoiceView) => void;
}): JSX.Element {
  const { client, toast } = useApp();
  const [open, setOpen] = useState(false);
  const [catalog, setCatalog] = useState<VoiceCatalog | null>(null);
  const [speed, setSpeed] = useState<number | null>(null);
  const [testing, setTesting] = useState(false);
  const [providers, setProviders] = useState<Provider[]>([]);
  const wrap = useRef<HTMLSpanElement>(null);
  const saving = useRef(Promise.resolve());
  const { view } = props;
  const id = props.agent.id;
  const elevenlabs = view?.provider === "elevenlabs";

  useDismiss(wrap, open, (escape) => {
    setOpen(false);
    if (escape) wrap.current?.querySelector("button")?.focus();
  });
  useEffect(() => {
    if (!open) return;
    setSpeed(null);
    wrap.current?.querySelector<HTMLElement>(".voice-quick select, .voice-quick input")?.focus();
    if (elevenlabs) client.voiceCatalog().then(setCatalog, () => undefined);
    client.listProviders().then(setProviders, () => undefined);
  }, [open]);

  const save = (changes: Partial<UpdateVoice>): void => {
    saving.current = saving.current.then(async () => {
      try {
        props.onView(await patchVoice(client, changes));
      } catch (err) {
        toast(err instanceof Error ? err.message : String(err));
      }
    });
  };
  const test = async (): Promise<void> => {
    setTesting(true);
    try {
      const url = URL.createObjectURL(
        await client.speech(`Hi, I'm ${props.agent.name}. This is how I sound.`, id),
      );
      const audio = new Audio(url);
      audio.onended = () => {
        URL.revokeObjectURL(url);
      };
      await audio.play();
    } catch (err) {
      toast(err instanceof Error ? err.message : String(err));
    } finally {
      setTesting(false);
    }
  };

  const mine = view?.agent_voices?.[id] ?? "";
  const callModel = view?.call_models?.[id] ?? "";
  const provider = providers.find((p) => p.id === props.agent.provider_id);
  const others = (byAgent: Record<string, string> | undefined): Record<string, string> =>
    Object.fromEntries(Object.entries(byAgent ?? {}).filter(([agent]) => agent !== id));
  const voices: [string, string][] =
    catalog?.voices.map((v) => [
      v.voice_id,
      `${v.name}${v.languages.length ? ` · ${v.languages.join(", ")}` : ""}`,
    ]) ?? [];
  const shownSpeed = speed ?? view?.speed ?? 1;
  return (
    <span class="voice-quick-wrap" ref={wrap}>
      <IconButton
        label="Voice, language and speed"
        expanded={open}
        onClick={() => {
          setOpen(!open);
        }}
      >
        <SlidersHorizontal size={18} />
      </IconButton>
      {open && view && (
        <div class="voice-quick" role="dialog" aria-label={`Voice of ${props.agent.name}`}>
          <header class="voice-quick-head">
            <span class="ellipsis">Voice of {props.agent.name}</span>
            <IconButton label="Test voice" disabled={testing} onClick={() => void test()}>
              <Play size={16} />
            </IconButton>
          </header>
          {elevenlabs && (
            <label class="pop-field">
              Voice
              <select
                value={mine}
                onChange={(e) => {
                  const v = e.currentTarget.value;
                  const rest = others(view.agent_voices);
                  save({ agent_voices: v ? { ...rest, [id]: v } : rest });
                }}
              >
                <option value="">Default voice</option>
                {withSaved(voices, mine).map(([v, label]) => (
                  <option key={v} value={v}>
                    {label}
                  </option>
                ))}
              </select>
            </label>
          )}
          <label class="pop-field">
            Language
            <select
              value={view.language ?? ""}
              onChange={(e) => {
                save({ language: e.currentTarget.value || null });
              }}
            >
              <option value="">Detect automatically</option>
              {withSaved(languagesOf(catalog, view.tts_model), view.language ?? "").map(
                ([code, name]) => (
                  <option key={code} value={code}>
                    {name}
                  </option>
                ),
              )}
            </select>
          </label>
          {elevenlabs && (
            <div class="pop-field">
              <span class="pop-label">
                Speed <output aria-hidden="true">{speedLabel(shownSpeed)}</output>
              </span>
              <Slider
                label="Speed"
                text={speedLabel(shownSpeed)}
                min={0.7}
                max={1.2}
                step={0.05}
                value={shownSpeed}
                onInput={setSpeed}
                onChange={(s) => {
                  save({ speed: s === 1 ? null : s });
                }}
              />
            </div>
          )}
          <label class="pop-field">
            Model in calls
            <ModelPicker
              lazy
              provider={props.agent.provider_id ?? undefined}
              kind={provider?.kind ?? "anthropic"}
              value={callModel}
              placeholder="Same as in chat"
              onChange={(m) => {
                const rest = others(view.call_models);
                save({ call_models: m.trim() ? { ...rest, [id]: m.trim() } : rest });
              }}
            />
          </label>
        </div>
      )}
    </span>
  );
}
