import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import {
  MessageSquare,
  Mic,
  MicOff,
  MicVocal,
  PhoneOff,
  Radio,
  Volume1,
  Volume2,
  VolumeX,
} from "lucide-preact";

import type { VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import {
  METER_DOTS,
  callClock,
  meterDots,
  nextSpeed,
  speedLabel,
  talkKey,
  volumeLabel,
} from "../lib/call.ts";
import { errorText } from "../lib/format.ts";
import { Avatar } from "./Avatar.tsx";
import { IconButton, Slider, useDismiss } from "./ui.tsx";
import { patchVoice } from "./VoiceSettings.tsx";

/** The running call as a bar above every view: who is in it, the level, its length and controls. */
export function CallBar(): JSX.Element | null {
  const { call, callState: state, agents, openAgent, openRoom, client, toast } = useApp();
  const [, redraw] = useState(0);
  const [voice, setVoice] = useState<VoiceView | null>(null);
  const [volumeOpen, setVolumeOpen] = useState(false);
  const volumeBox = useRef<HTMLSpanElement>(null);
  const saving = useRef(Promise.resolve());
  useDismiss(volumeBox, volumeOpen, (escape) => {
    setVolumeOpen(false);
    if (escape) volumeBox.current?.querySelector("button")?.focus();
  });
  useEffect(() => {
    if (volumeOpen) volumeBox.current?.querySelector("input")?.focus();
  }, [volumeOpen]);
  // Captions, the level and the speaking agent change often; only this bar follows them.
  useEffect(
    () =>
      call.subscribe(() => {
        redraw((n) => n + 1);
      }),
    [call],
  );
  const running = state !== null && !state.ending;
  useEffect(() => {
    if (!running) return;
    client.voice().then(setVoice, () => undefined);
    const tick = setInterval(() => {
      redraw((n) => n + 1);
    }, 1000);
    return () => {
      clearInterval(tick);
    };
  }, [running, client]);
  const pushToTalk = state?.hearing === true && call.pushToTalk;
  useEffect(() => {
    if (!pushToTalk) return;
    const down = (e: KeyboardEvent): void => {
      const el = e.target instanceof HTMLElement ? e.target : null;
      if (!talkKey(e, { tag: el?.tagName ?? "", editable: el?.isContentEditable === true })) return;
      if (document.querySelector('[role="dialog"], [role="menu"], [role="listbox"]')) return;
      // Also keeps Space from clicking a focused button, such as Hang up.
      e.preventDefault();
      call.talk(true);
    };
    const up = (e: KeyboardEvent): void => {
      if (e.code !== "Space" || !call.held) return;
      e.preventDefault();
      call.talk(false);
    };
    const release = (): void => {
      call.talk(false);
    };
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    window.addEventListener("blur", release);
    document.addEventListener("visibilitychange", release);
    return () => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
      window.removeEventListener("blur", release);
      document.removeEventListener("visibilitychange", release);
    };
  }, [pushToTalk, call]);
  if (!state) return null;

  const changeSpeed = (): void => {
    if (!voice) return;
    const speed = nextSpeed(voice.speed ?? null);
    setVoice({ ...voice, speed });
    // One after another, each on fresh settings: the update replaces all of them.
    saving.current = saving.current.then(async () => {
      try {
        await patchVoice(client, { speed: speed === 1 ? null : speed });
      } catch (err) {
        toast(errorText(err));
        client.voice().then(setVoice, () => undefined);
      }
    });
  };

  // An agent that just left stays on the bar while its goodbye plays.
  const ids =
    call.speaking && !state.agents.includes(call.speaking)
      ? [...state.agents, call.speaking]
      : state.agents;
  const people = ids.flatMap((id) => agents.get(id) ?? []);
  const host = agents.get(state.host);
  const room = state.room;
  const speaker = call.speaking ? agents.get(call.speaking) : undefined;
  const status = speaker
    ? `${speaker.name} is speaking`
    : state.ending
      ? "Call ended"
      : state.muted
        ? "Muted"
        : state.hearing
          ? call.caption || (pushToTalk && !call.held ? "Hold Space to talk" : "Listening…")
          : "Replies are read aloud";
  const lit = meterDots(call.level);
  const speed = speedLabel(voice?.speed ?? null);
  const volume = call.volume;

  return (
    <section
      class="call-bar"
      aria-label={
        people.length ? `Voice call with ${people.map((a) => a.name).join(", ")}` : "Voice call"
      }
    >
      <span class="call-faces">
        {people.map((a) => (
          <span
            key={a.id}
            class={`call-face${call.speaking === a.id ? " speaking" : ""}`}
            title={a.name}
          >
            <Avatar agent={a} size={28} />
          </span>
        ))}
      </span>
      <span class={`call-meter${call.speaking ? " playing" : ""}`} aria-hidden="true">
        {Array.from({ length: METER_DOTS }, (_, i) => (
          <i key={i} class={i < lit ? "on" : undefined} style={`--i: ${String(i)}`} />
        ))}
      </span>
      <span class="call-status ellipsis" role="status">
        {status}
      </span>
      <span class="call-clock">{state.ending ? "Ended" : callClock(Date.now() - state.since)}</span>
      <span class="call-actions">
        {room !== null ? (
          <IconButton
            label="Open the call room"
            onClick={() => {
              openRoom(room);
            }}
          >
            <MessageSquare size={16} />
          </IconButton>
        ) : (
          host && (
            <IconButton
              label={`Open the chat with ${host.name}`}
              onClick={() => {
                openAgent(host.id);
              }}
            >
              <MessageSquare size={16} />
            </IconButton>
          )
        )}
        {voice?.provider === "elevenlabs" && (
          <button
            type="button"
            class="icon-btn call-speed"
            aria-label={`Speaking speed ${speed}`}
            title={`Speaking speed ${speed}`}
            onClick={changeSpeed}
          >
            {speed}
          </button>
        )}
        <span class="call-volume" ref={volumeBox}>
          <IconButton
            label={`Call volume, ${volumeLabel(volume)}`}
            expanded={volumeOpen}
            onClick={() => {
              setVolumeOpen(!volumeOpen);
            }}
          >
            {volume === 0 ? (
              <VolumeX size={16} />
            ) : volume < 0.5 ? (
              <Volume1 size={16} />
            ) : (
              <Volume2 size={16} />
            )}
          </IconButton>
          {volumeOpen && (
            <span class="call-volume-pop">
              <span class="call-volume-track">
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
              </span>
              <output aria-hidden="true">{Math.round(volume * 100)}</output>
            </span>
          )}
        </span>
        {state.hearing && (
          <button
            type="button"
            class="icon-btn call-ptt"
            aria-label="Push to talk"
            title="Push to talk"
            aria-pressed={call.pushToTalk}
            onClick={() => {
              call.setPushToTalk(!call.pushToTalk);
            }}
          >
            <Radio size={16} />
          </button>
        )}
        {pushToTalk && (
          <button
            type="button"
            class="icon-btn call-talk"
            aria-label="Hold to talk (Space)"
            title="Hold to talk (Space)"
            aria-pressed={call.held}
            disabled={state.muted}
            onPointerDown={(e) => {
              call.talk(true);
              e.currentTarget.setPointerCapture(e.pointerId);
            }}
            onPointerUp={() => {
              call.talk(false);
            }}
            onPointerCancel={() => {
              call.talk(false);
            }}
            onLostPointerCapture={() => {
              call.talk(false);
            }}
          >
            <MicVocal size={16} />
          </button>
        )}
        <IconButton
          label="Mute"
          pressed={state.muted}
          disabled={!state.hearing}
          onClick={() => {
            call.mute(!state.muted);
          }}
        >
          {state.muted ? <MicOff size={16} /> : <Mic size={16} />}
        </IconButton>
        <IconButton
          label="Hang up"
          tone="danger"
          onClick={() => {
            call.hangUp();
          }}
        >
          <PhoneOff size={16} />
        </IconButton>
      </span>
    </section>
  );
}
