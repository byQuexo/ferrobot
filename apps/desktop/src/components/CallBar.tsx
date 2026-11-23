import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { MessageSquare, Mic, MicOff, PhoneOff, Square } from "lucide-preact";

import { useApp } from "../context.ts";
import { METER_DOTS, callClock, meterDots } from "../lib/call.ts";
import { Avatar } from "./Avatar.tsx";
import { IconButton } from "./ui.tsx";

/** The running call as a bar above every view: who is in it, the level, its length and controls. */
export function CallBar(): JSX.Element | null {
  const { call, callState: state, agents, openAgent, openRoom } = useApp();
  const [, redraw] = useState(0);
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
    const tick = setInterval(() => {
      redraw((n) => n + 1);
    }, 1000);
    return () => {
      clearInterval(tick);
    };
  }, [running]);
  if (!state) return null;

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
          ? call.caption || "Listening…"
          : "Replies are read aloud";
  const lit = meterDots(call.level);

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
          label="Stop speaking"
          disabled={!call.speaking}
          onClick={() => {
            call.quiet();
          }}
        >
          <Square size={14} />
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
