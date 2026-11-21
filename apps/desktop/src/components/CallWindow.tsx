import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { MessageSquare, Mic, MicOff, PhoneOff } from "lucide-preact";

import { useApp } from "../context.ts";
import { callClock } from "../lib/call.ts";
import { Avatar } from "./Avatar.tsx";
import { IconButton } from "./ui.tsx";

/** The running call: who is in it and who speaks, its length, mute, hang up and a way back. */
export function CallWindow(): JSX.Element | null {
  const { call, callState: state, agents, openAgent, openRoom } = useApp();
  const [, redraw] = useState(0);
  // Captions and the speaking agent change often; only this window follows them.
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

  const people = state.agents.flatMap((id) => agents.get(id) ?? []);
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

  return (
    <section class="call-window" aria-label="Voice call">
      <div class="call-top">
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
        <span class="call-info">
          <strong class="ellipsis">{people.map((a) => a.name).join(", ") || "Voice call"}</strong>
          <span class="call-clock">
            {state.ending ? "Ended" : callClock(Date.now() - state.since)}
          </span>
        </span>
      </div>
      <p class="call-status ellipsis" role="status">
        {status}
      </p>
      <div class="call-actions">
        <IconButton
          label="Mute"
          pressed={state.muted}
          disabled={!state.hearing}
          onClick={() => {
            call.mute(!state.muted);
          }}
        >
          {state.muted ? <MicOff size={18} /> : <Mic size={18} />}
        </IconButton>
        {room !== null ? (
          <IconButton
            label="Open the call room"
            onClick={() => {
              openRoom(room);
            }}
          >
            <MessageSquare size={18} />
          </IconButton>
        ) : (
          host && (
            <IconButton
              label={`Open the chat with ${host.name}`}
              onClick={() => {
                openAgent(host.id);
              }}
            >
              <MessageSquare size={18} />
            </IconButton>
          )
        )}
        <IconButton
          label="Hang up"
          tone="danger"
          onClick={() => {
            call.hangUp();
          }}
        >
          <PhoneOff size={18} />
        </IconButton>
      </div>
    </section>
  );
}
