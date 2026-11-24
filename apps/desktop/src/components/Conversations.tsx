import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { CornerDownRight, Eye, Zap } from "lucide-preact";

import type { Agent, Conversation, MailMessage } from "../api.ts";
import { useApp } from "../context.ts";
import { clockTime, relativeTime } from "../lib/format.ts";
import { mentionsAsText } from "../lib/mentions.ts";
import { Avatar } from "./Avatar.tsx";
import { RichText } from "./RichText.tsx";

export type Pair = readonly [string, string];

const isAgentMail = (m: MailMessage): boolean => m.sender.kind === "agent";

/** Conversations between agents, newest first; refreshed on new agent mail. */
export function ConversationList(props: {
  active: Pair | null;
  onOpen: (p: Pair) => void;
}): JSX.Element | null {
  const { client, agents, listen, nameOf } = useApp();
  const [list, setList] = useState<Conversation[]>([]);
  useEffect(() => {
    const load = (): void => {
      void client.listConversations().then(setList, () => undefined);
    };
    load();
    return listen((e) => {
      if (e.type === "message" && isAgentMail(e.message)) load();
    });
  }, [client, listen]);
  const known = list.filter((c) => agents.has(c.a) && agents.has(c.b));
  if (known.length === 0) return null;
  return (
    <section class="convo-section" aria-label="Conversations between agents">
      <h2 class="section-label">Conversations</h2>
      {known.map((c) => {
        const a = agents.get(c.a) as Agent;
        const b = agents.get(c.b) as Agent;
        const active = props.active?.[0] === c.a && props.active[1] === c.b;
        return (
          <button
            key={`${c.a}-${c.b}`}
            type="button"
            class={`convo-row${active ? " active" : ""}`}
            onClick={() => {
              props.onOpen([c.a, c.b]);
            }}
          >
            <span class="pair-avatars">
              <Avatar agent={a} size={22} />
              <Avatar agent={b} size={22} />
            </span>
            <span class="agent-row-text">
              <span class="agent-row-top">
                <span class="agent-name">
                  {a.name} · {b.name}
                </span>
                <span class="spacer" />
                <span class="meta">{relativeTime(c.last.created_at)}</span>
              </span>
              <span class="agent-desc">{mentionsAsText(c.last.body, nameOf)}</span>
            </span>
          </button>
        );
      })}
    </section>
  );
}

/** One pair's thread. Read-only: the operator watches, agents talk. */
export function ConversationView(props: { pair: Pair }): JSX.Element {
  const { client, agents, listen, run, nameOf } = useApp();
  const [a, b] = props.pair;
  const [messages, setMessages] = useState<MailMessage[]>([]);
  const scroller = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const load = (): void => {
      void run(() => client.conversation(a, b)).then((m) => {
        if (m) setMessages(m);
      });
    };
    load();
    return listen((e) => {
      if (e.type !== "message" || e.message.sender.kind !== "agent") return;
      const ids = [e.message.sender.id, e.message.recipient];
      if (ids.includes(a) && ids.includes(b)) load();
    });
  }, [client, listen, run, a, b]);
  useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages]);

  const left = agents.get(a);
  const right = agents.get(b);
  if (!left || !right)
    return (
      <section class="chat chat-none">
        <p class="muted">This conversation is no longer available.</p>
      </section>
    );
  const byId = new Map(messages.map((m) => [m.id, m]));
  return (
    <section class="chat" aria-label={`Conversation between ${left.name} and ${right.name}`}>
      <header class="chat-head">
        <span class="pair-avatars">
          <Avatar agent={left} size={30} />
          <Avatar agent={right} size={30} />
        </span>
        <div class="chat-title">
          <h1>
            {left.name} · {right.name}
          </h1>
          <span class="chat-sub">{messages.length} messages between agents</span>
        </div>
        <span class="spacer" />
        <span class="readonly-pill">
          <Eye size={16} /> Read-only
        </span>
      </header>
      <div class="timeline" ref={scroller}>
        {messages.map((m) => {
          const from = m.sender.kind === "agent" ? agents.get(m.sender.id) : undefined;
          const side = from?.id === a ? "row-agent" : "row-user row-peer";
          const parent = m.reply_to ? byId.get(m.reply_to) : undefined;
          return (
            <div key={m.id} class={`row ${side}`}>
              {from && from.id === a && <Avatar agent={from} size={28} />}
              <div class="bubble bubble-agent">
                {parent && (
                  <span class="reply-ref">
                    <CornerDownRight size={12} /> {mentionsAsText(parent.body, nameOf).slice(0, 80)}
                  </span>
                )}
                <RichText text={m.body} />
                <span class="meta">
                  {m.priority === true && (
                    <span class="urgent">
                      <Zap size={12} /> urgent ·{" "}
                    </span>
                  )}
                  {m.priority === false && "when free · "}
                  {from?.name ?? "agent"} · {clockTime(m.created_at)}
                  {m.status === "pending"
                    ? " · queued"
                    : m.status === "failed"
                      ? " · not delivered"
                      : ""}
                </span>
              </div>
              {from && from.id === b && <Avatar agent={from} size={28} />}
            </div>
          );
        })}
      </div>
    </section>
  );
}
