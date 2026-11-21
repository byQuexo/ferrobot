import type { JSX } from "preact";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";

import type { Room, RoomTranscript, VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import { roomTitle } from "../lib/call.ts";
import { clockTime, relativeTime } from "../lib/format.ts";
import { Avatar } from "./Avatar.tsx";
import { Composer } from "./Chat.tsx";
import { RichText } from "./RichText.tsx";

/** Rooms of calls with two or more agents, newest first. */
export function RoomList(props: {
  active: number | null;
  onOpen: (id: number) => void;
}): JSX.Element | null {
  const { client, agents, listen } = useApp();
  const [rooms, setRooms] = useState<Room[]>([]);
  useEffect(() => {
    const load = (): void => {
      void client.rooms().then(setRooms, () => undefined);
    };
    load();
    return listen((e) => {
      if (e.type === "room") load();
    });
  }, [client, listen]);
  if (rooms.length === 0) return null;
  const nameOf = (id: string): string | undefined => agents.get(id)?.name;
  return (
    <section class="convo-section" aria-label="Calls">
      <h2 class="section-label">Calls</h2>
      {rooms.map((r) => (
        <button
          key={r.id}
          type="button"
          class={`convo-row${props.active === r.id ? " active" : ""}`}
          onClick={() => {
            props.onOpen(r.id);
          }}
        >
          <span class="pair-avatars">
            {r.members.slice(0, 2).flatMap((id) => {
              const a = agents.get(id);
              return a ? [<Avatar key={id} agent={a} size={22} />] : [];
            })}
          </span>
          <span class="agent-row-text">
            <span class="agent-row-top">
              <span class="agent-name">{roomTitle(r.members, nameOf)}</span>
              <span class="spacer" />
              <span class="meta">{relativeTime(r.created_at)}</span>
            </span>
          </span>
        </button>
      ))}
    </section>
  );
}

/** A room's transcript; while it is the running call's room, the operator talks to the call here. */
export function RoomView(props: { id: number }): JSX.Element {
  const { client, agents, listen, run, call, callState } = useApp();
  const [room, setRoom] = useState<RoomTranscript | null>(null);
  const [voice, setVoice] = useState<VoiceView | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  useEffect(() => {
    client.voice().then(setVoice, () => undefined);
  }, [client]);
  useEffect(() => {
    const load = (): void => {
      void run(() => client.room(props.id)).then((r) => {
        if (r) setRoom(r);
      });
    };
    load();
    return listen((e) => {
      if (
        (e.type === "room_line" && e.line.room_id === props.id) ||
        (e.type === "room" && e.room.id === props.id)
      )
        load();
    });
  }, [client, listen, run, props.id]);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [room?.lines.length]);

  if (!room) return <section class="chat chat-none" aria-busy="true" />;
  const nameOf = (id: string): string | undefined => agents.get(id)?.name;
  const title = roomTitle(room.members, nameOf);
  const live = callState !== null && !callState.ending && callState.room === props.id;
  return (
    <section class="chat" aria-label={title}>
      <header class="chat-head">
        <span class="pair-avatars">
          {room.members.slice(0, 3).flatMap((id) => {
            const a = agents.get(id);
            return a ? [<Avatar key={id} agent={a} size={30} />] : [];
          })}
        </span>
        <div class="chat-title">
          <h1>{title}</h1>
          <span class="chat-sub">
            {room.ended_at ? `Ended ${clockTime(room.ended_at)}` : "Call in progress"}
          </span>
        </div>
      </header>
      <div class="timeline" ref={scroller}>
        {room.lines.map((l) => {
          if (l.speaker.kind !== "agent")
            return (
              <div key={l.id} class="row row-user">
                <div class="bubble bubble-user">
                  <RichText text={l.body} />
                  <span class="meta">You · {clockTime(l.created_at)}</span>
                </div>
              </div>
            );
          const from = agents.get(l.speaker.id);
          if (l.invited)
            return (
              <div key={l.id} class="row row-center">
                <div class="notice">
                  {from?.name ?? "An agent"} invited {nameOf(l.invited) ?? "an agent"} ·{" "}
                  {clockTime(l.created_at)}
                </div>
              </div>
            );
          return (
            <div key={l.id} class="row row-agent">
              {from && <Avatar agent={from} size={28} />}
              <div class="bubble bubble-agent">
                <RichText text={l.body} />
                <span class="meta">
                  {from?.name ?? "agent"} · {clockTime(l.created_at)}
                </span>
              </div>
            </div>
          );
        })}
      </div>
      {live && (
        <Composer
          chat={`room-${String(props.id)}`}
          placeholder="Say something to the call"
          send={(body) => call.say(body)}
          voice={voice?.enabled === false ? null : voice}
          onTalk={() => {
            call.quiet();
          }}
        />
      )}
    </section>
  );
}
