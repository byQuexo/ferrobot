import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";

import type { Agent, VoiceCatalog, VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import { Button } from "./ui.tsx";
import { languagesOf, voiceUpdate } from "./VoiceSettings.tsx";

/** Voice, language and speed for this chat, changed on the fly; saved right away. */
export function VoiceQuick(props: {
  agent: Agent;
  view: VoiceView;
  onView: (v: VoiceView) => void;
  onClose: () => void;
}): JSX.Element {
  const { client, toast } = useApp();
  const [catalog, setCatalog] = useState<VoiceCatalog | null>(null);
  const [speed, setSpeed] = useState(props.view.speed ?? 1);
  const box = useRef<HTMLDivElement>(null);
  const { view } = props;
  const id = props.agent.id;
  const elevenlabs = view.provider === "elevenlabs";

  useEffect(() => {
    if (elevenlabs) client.voiceCatalog().then(setCatalog, () => undefined);
    const outside = (e: PointerEvent): void => {
      if (!box.current?.contains(e.target as Node)) props.onClose();
    };
    document.addEventListener("pointerdown", outside);
    return () => {
      document.removeEventListener("pointerdown", outside);
    };
  }, []);

  const save = async (changes: Parameters<typeof voiceUpdate>[1]): Promise<void> => {
    try {
      props.onView(await client.updateVoice(voiceUpdate(view, changes)));
    } catch (err) {
      toast(err instanceof Error ? err.message : String(err));
    }
  };
  const test = async (): Promise<void> => {
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
    }
  };

  const mine = view.agent_voices?.[id] ?? "";
  return (
    <div class="voice-quick" ref={box} role="dialog" aria-label="Voice for this chat">
      {elevenlabs && (
        <label>
          Voice of {props.agent.name}
          <select
            value={mine}
            onChange={(e) => {
              const v = e.currentTarget.value;
              const voices = { ...(view.agent_voices ?? {}) };
              if (v) voices[id] = v;
              else delete voices[id];
              void save({ agent_voices: voices });
            }}
          >
            <option value="">Default voice</option>
            {catalog?.voices.map((v) => (
              <option key={v.voice_id} value={v.voice_id}>
                {v.name}
                {v.languages.length ? ` · ${v.languages.join(", ")}` : ""}
              </option>
            ))}
            {mine && !catalog?.voices.some((v) => v.voice_id === mine) && (
              <option value={mine}>{mine}</option>
            )}
          </select>
        </label>
      )}
      <label>
        Language
        <select
          value={view.language ?? ""}
          onChange={(e) => void save({ language: e.currentTarget.value || null })}
        >
          <option value="">Detect automatically</option>
          {languagesOf(catalog, view.tts_model).map(([code, name]) => (
            <option key={code} value={code}>
              {name}
            </option>
          ))}
        </select>
      </label>
      {elevenlabs && (
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
            onChange={() => void save({ speed: speed === 1 ? null : speed })}
          />
        </label>
      )}
      <Button small variant="secondary" onClick={() => void test()}>
        Test voice
      </Button>
    </div>
  );
}
