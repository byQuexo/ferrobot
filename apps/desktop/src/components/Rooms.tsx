import type { JSX } from "preact";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import { Pencil, Phone, PhoneOff } from "lucide-preact";

import type { Room, RoomTranscript, VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import { roomTitle } from "../lib/call.ts";
import { characterOf } from "../lib/character.ts";
import { clockTime, relativeTime } from "../lib/format.ts";
import { Avatar } from "./Avatar.tsx";
import { Composer } from "./Chat.tsx";
import { GroupDialog } from "./Groups.tsx";
import { RichText } from "./RichText.tsx";
import { IconButton } from "./ui.tsx";

/** Rooms of calls with two or more agents, newest first. */
export function RoomList(props: {
  active: number | null;
  onOpen: (id: number) => void;
}): JSX.Element | null {
  const { client, agents, listen, nameOf } = useApp();
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
  // A room outlives deleted agents; one with none left has nothing to show.
  const shown = rooms.filter((r) => r.members.some((id) => agents.has(id)));
  if (shown.length === 0) return null;
  return (
    <section class="convo-section" aria-label="Calls">
      <h2 class="section-label">Calls</h2>
      {shown.map((r) => (
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

/**
 * A room's transcript; while it is the running call's room, the operator talks to the call here.
 * A group (a named room) always has a composer, a call button and its settings.
 */
export function RoomView(props: { id: number; onClose: () => void }): JSX.Element {
  const { client, agents, listen, run, call, callState, nameOf } = useApp();
  const [room, setRoom] = useState<RoomTranscript | null>(null);
  const [voice, setVoice] = useState<VoiceView | null>(null);
  const [editing, setEditing] = useState(false);
  const [speaking, setSpeaking] = useState<string | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  useEffect(() => {
    client.voice().then(setVoice, () => undefined);
  }, [client]);
  useEffect(() => {
    const load = (): void => {
      void run(() => client.room(props.id)).then((r) => {
        if (!r) return;
        setRoom(r);
        setSpeaking(r.speaking ?? null);
      });
    };
    load();
    return listen((e) => {
      if (
        (e.type === "room_line" && e.line.room_id === props.id) ||
        (e.type === "room" && e.room.id === props.id)
      )
        load();
      if (e.type === "room_deleted" && e.room_id === props.id) props.onClose();
      if (e.type === "room_floor" && e.room_id === props.id) setSpeaking(e.agent ?? null);
    });
  }, [client, listen, run, props.id, props.onClose]);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [room?.lines.length, speaking]);

  if (!room) return <section class="chat chat-none" aria-busy="true" />;
  const group = room.name ?? null;
  const present = room.members.filter((id) => agents.has(id));
  const title = group ?? roomTitle(room.members, nameOf);
  const live = callState !== null && !callState.ending && callState.room === props.id;
  const speaker = speaking === null ? undefined : agents.get(speaking);
  const sub =
    group === null
      ? room.ended_at
        ? `Ended ${clockTime(room.ended_at)}`
        : "Call in progress"
      : `${live ? "In a call · " : ""}${present.flatMap((id) => nameOf(id) ?? []).join(", ")}`;
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
          <span class="chat-sub">{sub}</span>
        </div>
        {group !== null && (
          <>
            <span class="spacer" />
            <IconButton
              label={
                live
                  ? "End the call"
                  : callState
                    ? "End the current call first"
                    : present.length > 4
                      ? "A call fits four agents"
                      : "Call the group"
              }
              onClick={() => {
                if (live) call.hangUp();
                else if (present[0]) void call.start(present[0], props.id);
              }}
              pressed={live}
              disabled={!live && (callState !== null || present.length === 0 || present.length > 4)}
            >
              {live ? <PhoneOff size={18} /> : <Phone size={18} />}
            </IconButton>
            <IconButton
              label="Edit group"
              onClick={() => {
                setEditing(true);
              }}
            >
              <Pencil size={18} />
            </IconButton>
          </>
        )}
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
                <span
                  class="speaker"
                  style={from ? { "--char-hue": String(characterOf(from).hue) } : undefined}
                >
                  {from?.name ?? "agent"}
                </span>
                <RichText text={l.body} />
                <span class="meta">{clockTime(l.created_at)}</span>
              </div>
            </div>
          );
        })}
        {speaker && (
          <div class="row row-agent" role="status">
            <Avatar agent={speaker} size={28} />
            <span class="step-now">
              {speaker.name} is {live ? "speaking" : "writing"}…
            </span>
          </div>
        )}
      </div>
      {(live || group !== null) && (
        <Composer
          chat={`room-${String(props.id)}`}
          placeholder={live ? "Say something to the call" : "Write to the group"}
          send={(body) => (group === null ? call.say(body) : client.sayInGroup(props.id, body))}
          voice={voice?.enabled === false ? null : voice}
          onTalk={() => {
            call.quiet();
          }}
          mentions={group === null ? undefined : present.flatMap((id) => agents.get(id) ?? [])}
        />
      )}
      {editing && (
        <GroupDialog
          group={room}
          onClose={() => {
            setEditing(false);
          }}
          onSaved={() => {
            setEditing(false);
          }}
          onDeleted={props.onClose}
        />
      )}
    </section>
  );
}
