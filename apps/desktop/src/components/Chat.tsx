import type { JSX } from "preact";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "preact/hooks";
import {
  AlertTriangle,
  ArrowDownLeft,
  ArrowUpRight,
  CalendarClock,
  ChevronRight,
  CircleStop,
  MessagesSquare,
  Info,
  Pencil,
  SendHorizontal,
  ShieldAlert,
  Wrench,
} from "lucide-preact";

import type { Agent, MailMessage, Routine, StoredEvent } from "../api.ts";
import { useApp } from "../context.ts";
import { clockTime, presenceLabel, toolLabel } from "../lib/format.ts";
import { buildTimeline, groupExchanges, type DisplayItem, type Exchange, type TimelineItem } from "../lib/timeline.ts";
import { Avatar } from "./Avatar.tsx";
import { QuestionCardView } from "./QuestionCard.tsx";
import { RichText } from "./RichText.tsx";
import { Button, IconButton } from "./ui.tsx";

const PAGE = 500;

export function Chat(props: {
  agent: Agent;
  infoOpen: boolean;
  onToggleInfo: () => void;
  onEdit: () => void;
}): JSX.Element {
  const { client, agents, listen, run } = useApp();
  const agent = props.agent;
  const id = agent.id;
  const [messages, setMessages] = useState<MailMessage[]>([]);
  const [events, setEvents] = useState<StoredEvent[]>([]);
  const [routines, setRoutines] = useState<Routine[]>([]);
  const [live, setLive] = useState<Map<string, string>>(new Map());
  const [loaded, setLoaded] = useState(false);

  // Initial load + live updates for the selected agent.
  useEffect(() => {
    let cancelled = false;
    setLoaded(false);
    setMessages([]);
    setEvents([]);
    setLive(new Map());
    const load = async (): Promise<void> => {
      const all: StoredEvent[] = [];
      for (let page = 0, after = 0; page < 10; page++) {
        const batch = await client.events(id, after);
        all.push(...batch);
        if (batch.length < PAGE) break;
        after = batch[batch.length - 1]?.id ?? after;
      }
      const [msgs, rts] = await Promise.all([client.messages(id), client.routines(id)]);
      if (cancelled) return;
      setMessages(msgs);
      setEvents(all);
      setRoutines(rts);
      setLoaded(true);
      void client.markRead(id).catch(() => undefined);
    };
    void run(load);
    const off = listen((e) => {
      switch (e.type) {
        case "event":
          if (e.event.agent_id !== id) return;
          setEvents((prev) => (prev.some((x) => x.id === e.event.id) ? prev : [...prev, e.event]));
          if (e.event.turn_id && (e.event.kind === "assistant_text" || e.event.kind === "turn_done")) {
            const turn = e.event.turn_id;
            setLive((prev) => {
              if (!prev.has(turn)) return prev;
              const next = new Map(prev);
              next.delete(turn);
              return next;
            });
          }
          if (e.event.kind === "turn_done") void client.markRead(id).catch(() => undefined);
          break;
        case "delta":
          if (e.agent_id !== id) return;
          setLive((prev) => new Map(prev).set(e.turn_id, (prev.get(e.turn_id) ?? "") + e.text));
          break;
        case "message": {
          const m = e.message;
          const mine = m.recipient === id || (m.sender.kind === "agent" && m.sender.id === id);
          if (!mine) return;
          setMessages((prev) => (prev.some((x) => x.id === m.id) ? prev.map((x) => (x.id === m.id ? m : x)) : [...prev, m]));
          break;
        }
        case "routines_changed":
          if (e.agent_id === id) void client.routines(id).then(setRoutines).catch(() => undefined);
          break;
        default:
          break;
      }
    });
    return () => {
      cancelled = true;
      off();
    };
  }, [id]);

  const items = useMemo(() => {
    const names = {
      agents: new Map([...agents.values()].map((a) => [a.id, a.name])),
      routines: new Map(routines.map((r) => [r.id, r.name])),
    };
    return groupExchanges(buildTimeline(id, messages, events, live, names));
  }, [id, messages, events, live, agents, routines]);

  // Keep the view pinned to the bottom unless the user scrolled up.
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  }, [items]);

  const working = agent.presence === "working";

  return (
    <section class="chat" aria-label={`Chat with ${agent.name}`}>
      <header class="chat-head">
        <Avatar agent={agent} size={38} presence={agent.presence} />
        <div class="chat-title">
          <h1>{agent.name}</h1>
          <span class={`chat-sub p-${agent.presence}`}>
            <span class="dot" />
            {presenceLabel(agent.presence)}
            {agent.description && <span class="muted"> · {agent.description}</span>}
          </span>
        </div>
        <span class="spacer" />
        {working && (
          <Button variant="secondary" small onClick={() => void run(() => client.interrupt(id))}>
            <CircleStop size={15} /> Stop turn
          </Button>
        )}
        <IconButton label="Edit agent" onClick={props.onEdit}>
          <Pencil size={17} />
        </IconButton>
        <IconButton label={props.infoOpen ? "Hide details" : "Show details"} onClick={props.onToggleInfo} pressed={props.infoOpen}>
          <Info size={18} />
        </IconButton>
      </header>

      <div
        class="timeline"
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget;
          pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
        }}
      >
        {loaded && items.length === 0 && (
          <div class="chat-empty">
            <Avatar agent={agent} size={64} />
            <p class="empty-title">Say hello to {agent.name}</p>
            <p class="muted">
              Messages are delivered to the agent’s own computer. It wakes up automatically when something arrives.
            </p>
          </div>
        )}
        {items.map((item) => (
          <Item key={item.key} item={item} agent={agent} />
        ))}
        {working && !items.some((i) => i.type === "assistant" && i.streaming) && (
          <div class="row row-agent">
            <Avatar agent={agent} size={28} />
            <div class="typing" aria-label={`${agent.name} is working`}>
              <span />
              <span />
              <span />
            </div>
          </div>
        )}
      </div>

      <Composer agent={agent} />
    </section>
  );
}

/** "N messages with <peer>": collapsed agent-to-agent mail, expandable inline. */
function ExchangeHint(props: { item: Exchange; agent: Agent }): JSX.Element {
  const { item, agent } = props;
  const { agents, openConversation } = useApp();
  const [open, setOpen] = useState(false);
  const peer = agents.get(item.peerId);
  const n = item.messages.length;
  return (
    <>
      <div class={`row row-center exchange${open ? " open" : ""}`}>
        <button type="button" class="exchange-hint" aria-expanded={open} onClick={() => { setOpen(!open); }}>
          <span>
            {n} {n === 1 ? "message" : "messages"} with
          </span>
          {peer && <Avatar agent={peer} size={18} />}
          <strong>{peer?.name ?? item.peerName}</strong>
          <ChevronRight size={14} class="chev" />
        </button>
        {peer && (
          <button
            type="button"
            class="exchange-open"
            title={`Open the thread between ${agent.name} and ${peer.name}`}
            aria-label="Open thread"
            onClick={() => { openConversation(agent.id, peer.id); }}
          >
            <MessagesSquare size={14} />
          </button>
        )}
      </div>
      {open && [...item.tools, ...item.messages]
        .sort((a, b) => Date.parse(a.at) - Date.parse(b.at))
        .map((m) => <Item key={m.key} item={m} agent={agent} />)}
    </>
  );
}

function Item(props: { item: DisplayItem; agent: Agent }): JSX.Element {
  const { item, agent } = props;
  const { agents } = useApp();
  switch (item.type) {
    case "exchange":
      return <ExchangeHint item={item} agent={agent} />;
    case "user":
      return (
        <div class="row row-user">
          <div class="bubble bubble-user">
            <RichText text={item.text} />
            <span class="meta">
              {clockTime(item.at)}
              {item.status === "pending" ? " · queued" : item.status === "failed" ? " · not delivered" : ""}
            </span>
          </div>
        </div>
      );
    case "assistant":
      return (
        <div class="row row-agent">
          <Avatar agent={agent} size={28} />
          <div class={`bubble-agent${item.streaming ? " streaming" : ""}`}>
            <span class="agent-msg-head">
              <strong>{agent.name}</strong>
              {!item.streaming && <span class="meta">{clockTime(item.at)}</span>}
            </span>
            <RichText text={item.text} />
          </div>
        </div>
      );
    case "incoming":
    case "outgoing": {
      const peerName = item.type === "incoming" ? item.from : item.to;
      const peer = agents.get(item.peerId);
      return (
        <div class={`row row-a2a ${item.type}`}>
          <div class="a2a">
            <div class="a2a-head">
              {item.type === "incoming" ? <ArrowDownLeft size={14} /> : <ArrowUpRight size={14} />}
              {peer ? <Avatar agent={peer} size={18} /> : null}
              <span>
                {item.type === "incoming" ? (
                  <>
                    From <strong>{peerName}</strong>
                  </>
                ) : (
                  <>
                    {agent.name} → <strong>{peerName}</strong>
                  </>
                )}
              </span>
              <span class="spacer" />
              <span class="meta">{clockTime(item.at)}</span>
            </div>
            <RichText text={item.text} />
          </div>
        </div>
      );
    }
    case "routine":
      return (
        <div class="row row-routine">
          <div class="routine-msg">
            <div class="a2a-head">
              <CalendarClock size={14} />
              <span>
                Routine <strong>{item.routine}</strong>
              </span>
              <span class="spacer" />
              <span class="meta">{clockTime(item.at)}</span>
            </div>
            <RichText text={item.text} />
          </div>
        </div>
      );
    case "tool":
      return <ToolCard item={item} />;
    case "question":
      return <QuestionCardView agent={agent} id={item.id} card={item.card} answered={item.answered} />;
    case "error":
      return (
        <div class="row row-center">
          <div class="notice notice-error">
            <AlertTriangle size={14} /> {item.text}
          </div>
        </div>
      );
    case "system":
      return (
        <div class="row row-center">
          <div class="notice">
            {item.text} · {clockTime(item.at)}
          </div>
        </div>
      );
  }
}

function pretty(value: unknown): string {
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/** Pretty-prints JSON tool output; other text is shown as is. */
function prettyOutput(text: string): string {
  const t = text.trim();
  if (!t.startsWith("{") && !t.startsWith("[")) return text;
  try {
    return JSON.stringify(JSON.parse(t), null, 2);
  } catch {
    return text;
  }
}

/** One-line summary of a tool input, e.g. `to: bob, body: "hi"`. */
function summary(input: unknown): string {
  if (input === null || typeof input !== "object") return "";
  return Object.entries(input as Record<string, unknown>)
    .slice(0, 3)
    .map(([k, v]) => `${k}: ${typeof v === "string" ? v : JSON.stringify(v)}`)
    .join(", ");
}

function ToolCard(props: { item: Extract<TimelineItem, { type: "tool" }> }): JSX.Element {
  const { item } = props;
  const [open, setOpen] = useState(false);
  const label = toolLabel(item.name);
  const state = item.denied !== null ? "denied" : item.output === null ? "running" : item.isError ? "error" : "ok";
  return (
    <div class="row row-tool">
      <div class={`tool-card t-${state}${open ? " open" : ""}`}>
        <button type="button" class="tool-head" aria-expanded={open} onClick={() => { setOpen(!open); }}>
          <ChevronRight size={14} class="chev" />
          {state === "denied" ? <ShieldAlert size={14} /> : <Wrench size={14} />}
          <span class="tool-name">{label.tool}</span>
          {label.server && <span class="tool-server">{label.server}</span>}
          <span class="tool-summary">{state === "denied" ? `Blocked: ${item.denied ?? ""}` : summary(item.input)}</span>
          <span class={`tool-state s-${state}`}>
            {state === "running" ? "running" : state === "ok" ? "done" : state === "error" ? "error" : "blocked"}
          </span>
        </button>
        {open && (
          <div class="tool-body">
            {item.input !== null && (
              <>
                <div class="tool-label">Input</div>
                <pre>{pretty(item.input)}</pre>
              </>
            )}
            {item.output !== null && (
              <>
                <div class="tool-label">{item.isError ? "Error" : "Result"}</div>
                <pre>{prettyOutput(item.output)}</pre>
              </>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function Composer(props: { agent: Agent }): JSX.Element {
  const { client, run } = useApp();
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => { ref.current?.focus(); }, [props.agent.id]);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${String(Math.min(el.scrollHeight, 200))}px`;
  }, [text]);

  const send = async (): Promise<void> => {
    const body = text.trim();
    if (!body || sending) return;
    setSending(true);
    const ok = await run(() => client.send(props.agent.id, body));
    setSending(false);
    if (ok) setText("");
  };
  const asleep = props.agent.presence === "sleeping";

  return (
    <form
      class="composer"
      onSubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      <textarea
        ref={ref}
        rows={1}
        value={text}
        placeholder={asleep ? `Message ${props.agent.name} — this wakes it up` : `Message ${props.agent.name}`}
        onInput={(e) => { setText(e.currentTarget.value); }}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
            e.preventDefault();
            void send();
          }
        }}
      />
      <button type="submit" class="send" disabled={!text.trim() || sending} aria-label="Send" title="Send (Enter)">
        <SendHorizontal size={18} />
      </button>
    </form>
  );
}
