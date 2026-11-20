import type { JSX } from "preact";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "preact/hooks";
import {
  FileText,
  AlertTriangle,
  ArrowDownLeft,
  ArrowUpRight,
  CalendarClock,
  ChevronRight,
  CircleStop,
  MessagesSquare,
  Info,
  Mic,
  Pencil,
  Phone,
  PhoneOff,
  SendHorizontal,
  ShieldAlert,
  SlidersHorizontal,
  Square,
  Wrench,
} from "lucide-preact";

import type { Agent, MailMessage, Routine, StoredEvent, VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import { clockTime, presenceLabel, stepLabel, toolLabel } from "../lib/format.ts";
import { buildTimeline, groupExchanges, sharedFiles, type DisplayItem, type Exchange, type TimelineItem } from "../lib/timeline.ts";
import { record, Speaker, speakable, startLive, type LiveSession, type Recording } from "../lib/voice.ts";
import { fileSize } from "./FileViewer.tsx";
import { Avatar } from "./Avatar.tsx";
import { QuestionCardView } from "./QuestionCard.tsx";
import { RichText, type ChatFiles } from "./RichText.tsx";
import { Button, IconButton } from "./ui.tsx";
import { VoiceQuick } from "./VoiceQuick.tsx";


export function Chat(props: {
  agent: Agent;
  infoOpen: boolean;
  onToggleInfo: () => void;
  onEdit: () => void;
}): JSX.Element {
  const { client, agents, listen, run, toast } = useApp();
  const agent = props.agent;
  const id = agent.id;
  const [messages, setMessages] = useState<MailMessage[]>([]);
  const [events, setEvents] = useState<StoredEvent[]>([]);
  const [routines, setRoutines] = useState<Routine[]>([]);
  const [live, setLive] = useState<Map<string, string>>(new Map());
  const [loaded, setLoaded] = useState(false);
  // Voice on = a voice call with this agent; agents in it may invite others.
  const [voice, setVoice] = useState(false);
  const [call, setCall] = useState<string[]>([]);
  // Read by the live-event listener, which outlives renders.
  const voiceOn = useRef(false);
  voiceOn.current = voice;
  const inCall = useRef(call);
  inCall.current = call;
  const names = useRef(agents);
  names.current = agents;
  const spoken = useRef(new Set<number>());
  const speaker = useMemo(
    () => new Speaker(client.speech, (err) => { toast(err instanceof Error ? err.message : String(err)); }),
    [client],
  );
  const [voiceView, setVoiceView] = useState<VoiceView | null>(null);
  const [quick, setQuick] = useState(false);
  // What the operator is saying during a hands-free call.
  const [caption, setCaption] = useState("");
  const listening = useRef<LiveSession | null>(null);
  const [hearing, setHearing] = useState(false);
  useEffect(() => { client.voice().then(setVoiceView, () => undefined); }, [client]);
  const voiceEnabled = voiceView?.enabled !== false;

  const endCall = (): void => {
    speaker.stop();
    speaker.onPlaying = () => {};
    void listening.current?.stop(false);
    listening.current = null;
    setHearing(false);
    setCaption("");
    setVoice(false);
    setCall([]);
  };
  const toggleCall = async (): Promise<void> => {
    if (voice) {
      endCall();
      void run(() => client.setCall([]));
      return;
    }
    const settings = await run(() => client.voice());
    if (!settings) return;
    setVoiceView(settings);
    const provider = settings.providers.find((p) => p.kind === settings.provider);
    if (!provider?.has_key || !(settings.voice ?? provider.default_voice)) {
      toast("Set up voice first: a key and a voice (Settings → Voice)", "info");
      return;
    }
    const agents = await run(() => client.setCall([id]));
    if (!agents) return;
    setCall(agents);
    setVoice(true);
    if (settings.provider !== "elevenlabs") {
      toast("Hands-free listening needs ElevenLabs; replies are read aloud, dictate with the microphone", "info");
      return;
    }
    try {
      listening.current = await startLive((rate) => client.voiceLive("call", rate), {
        partial: setCaption,
        committed: (text) => {
          setCaption("");
          const said = text.trim();
          if (said) void run(() => sendRef.current(said));
        },
        error: (message) => { toast(`Listening stopped: ${message}`); },
      });
      // The agent's own voice must not come back as something the operator said.
      speaker.onPlaying = (playing) => { listening.current?.mute(playing); };
      setHearing(true);
    } catch (err) {
      toast(err instanceof Error ? err.message : String(err));
    }
  };
  // Everyone in a call hears what the operator says; otherwise it goes to this agent only.
  const send = (body: string): Promise<unknown> =>
    voice && call.length > 1 ? client.sayInCall(body) : client.send(id, body);
  const sendRef = useRef(send);
  sendRef.current = send;

  // Initial load + live updates for the selected agent.
  useEffect(() => {
    let cancelled = false;
    setLoaded(false);
    setMessages([]);
    setEvents([]);
    setLive(new Map());
    const load = async (): Promise<void> => {
      const all = await client.allEvents(id);
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
        case "event": {
          const ev = e.event;
          const speaks = ev.agent_id === id || inCall.current.includes(ev.agent_id);
          if (voiceOn.current && speaks && ev.kind === "assistant_text" && !spoken.current.has(ev.id)) {
            spoken.current.add(ev.id);
            const text = (ev.payload as { text?: unknown }).text;
            if (typeof text === "string") speaker.say(speakable(text, (aid) => names.current.get(aid)?.name), ev.agent_id);
          }
          if (ev.agent_id !== id) return;
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
        }
        case "call":
          if (!voiceOn.current) return;
          if (e.agents.includes(id)) setCall(e.agents);
          else endCall();
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
      // Leaving the chat hangs up.
      if (voiceOn.current) void client.setCall([]).catch(() => undefined);
      endCall();
    };
  }, [id]);

  const items = useMemo(() => {
    const names = {
      agents: new Map([...agents.values()].map((a) => [a.id, a.name])),
      routines: new Map(routines.map((r) => [r.id, r.name])),
    };
    const all = buildTimeline(id, messages, events, live, names);
    // Tool calls and turn errors live on the Activities tab; the chat shows the conversation.
    const shown = groupExchanges(all.filter((i) => i.type !== "tool" && i.type !== "error"));
    // The current activity: the newest step since the message being worked on.
    let start = all.length - 1;
    while (start >= 0 && !["user", "incoming", "routine"].includes(all[start]?.type ?? "")) start--;
    const last = all.slice(start + 1).filter((i) => i.type === "tool").pop();
    const nameOf = (agentId: string): string | undefined => agents.get(agentId)?.name;
    const files: ChatFiles = { agent: id, byName: sharedFiles(events) };
    return { shown, files, step: last?.type === "tool" ? stepLabel(last.name, last.input, nameOf) : "Getting started" };
  }, [id, messages, events, live, agents, routines]);

  // Keep the view pinned to the bottom unless the user scrolled up.
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  }, [items.shown, items.step]);

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
        {voice && call.length > 1 && (
          <span class="call-people" title={`Also in the call: ${call.filter((a) => a !== id).map((a) => agents.get(a)?.name ?? "?").join(", ")}`}>
            {call.filter((a) => a !== id).map((a) => {
              const peer = agents.get(a);
              return peer ? <Avatar key={a} agent={peer} size={24} /> : null;
            })}
          </span>
        )}
        {voiceEnabled && (
          <span class="voice-tools">
            <IconButton label="Voice, language and speed" onClick={() => { setQuick(!quick); }} pressed={quick}>
              <SlidersHorizontal size={17} />
            </IconButton>
            {quick && voiceView && (
              <VoiceQuick agent={agent} view={voiceView} onView={setVoiceView} onClose={() => { setQuick(false); }} />
            )}
            <IconButton label={voice ? "End the call" : "Start a voice call"} onClick={() => void toggleCall()} pressed={voice}>
              {voice ? <PhoneOff size={18} /> : <Phone size={18} />}
            </IconButton>
          </span>
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
        {loaded && items.shown.length === 0 && (
          <div class="chat-empty">
            <Avatar agent={agent} size={64} />
            <p class="empty-title">Say hello to {agent.name}</p>
            <p class="muted">
              Messages are delivered to the agent’s own computer. It wakes up automatically when something arrives.
            </p>
          </div>
        )}
        {items.shown.map((item) => (
          <Item key={item.key} item={item} agent={agent} files={items.files} />
        ))}
        {working && !items.shown.some((i) => i.type === "assistant" && i.streaming) && (
          <div class="row row-agent">
            <Avatar agent={agent} size={28} mood="working" />
            <div class="step-now" role="status">{items.step}…</div>
          </div>
        )}
      </div>

      {voice && hearing && (
        <div class="call-caption" role="status">
          <Mic size={14} /> {caption || "Listening…"}
        </div>
      )}
      <Composer agent={agent} send={send} voice={voiceEnabled ? voiceView : null} onTalk={() => { speaker.stop(); }} />
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

function Item(props: { item: DisplayItem; agent: Agent; files?: ChatFiles }): JSX.Element {
  const { item, agent } = props;
  const { agents, openFile } = useApp();
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
          <div class={`bubble-agent${item.streaming ? " streaming" : ""}`}>
            <RichText text={item.text} files={props.files} />
            {!item.streaming && <span class="meta">{clockTime(item.at)}</span>}
          </div>
        </div>
      );
    case "file":
      return (
        <div class="row row-agent">
          <button type="button" class="file-card" onClick={() => { openFile(agent.id, item.id); }}>
            <FileText size={18} />
            <span class="file-name">{item.name}</span>
            <span class="meta">{fileSize(item.size)} · {clockTime(item.at)}</span>
          </button>
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

export function ToolCard(props: { item: Extract<TimelineItem, { type: "tool" }> }): JSX.Element {
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

/** Joins dictated pieces with single spaces. */
const joinText = (...parts: string[]): string => parts.map((p) => p.trim()).filter(Boolean).join(" ");

/**
 * Dictation: hold to talk, or click to start and again to stop. The words
 * appear in the message box as they are said (ElevenLabs) or after stopping
 * (other providers); nothing is sent until the operator sends it.
 */
function MicButton(props: {
  agent: Agent;
  voice: VoiceView;
  text: () => string;
  setText: (t: string) => void;
  onTalk: () => void;
}): JSX.Element {
  const { client, run, toast } = useApp();
  const [recording, setRecording] = useState(false);
  const [transcribing, setTranscribing] = useState(false);
  const rec = useRef<Recording | null>(null);
  const session = useRef<LiveSession | null>(null);
  const pressedAt = useRef(0);
  const limit = useRef<ReturnType<typeof setTimeout>>();
  // Bumped when the chat changes, so a recording that starts late is dropped.
  const chat = useRef(0);

  const finish = async (): Promise<void> => {
    clearTimeout(limit.current);
    setRecording(false);
    const s = session.current;
    if (s) {
      session.current = null;
      await s.stop(true);
      return;
    }
    const r = rec.current;
    if (!r) return;
    rec.current = null;
    setTranscribing(true);
    const before = props.text();
    const text = await run(async () => client.transcribe(await r.stop()));
    setTranscribing(false);
    if (text === undefined) return;
    if (!text) toast("Nothing heard", "info");
    else props.setText(joinText(before, text));
  };
  const start = async (): Promise<void> => {
    props.onTalk();
    const startedIn = chat.current;
    const before = props.text();
    try {
      if (props.voice.provider === "elevenlabs") {
        let said = "";
        const s = await startLive((rate) => client.voiceLive("dictation", rate), {
          partial: (p) => { props.setText(joinText(before, said, p)); },
          committed: (c) => {
            said = joinText(said, c);
            props.setText(joinText(before, said));
          },
          error: (message) => { toast(`Dictation stopped: ${message}`); },
        });
        if (startedIn !== chat.current) {
          void s.stop(false);
          return;
        }
        session.current = s;
      } else {
        const r = await record();
        if (startedIn !== chat.current) {
          void r.stop();
          return;
        }
        rec.current = r;
      }
      setRecording(true);
      limit.current = setTimeout(() => void finish(), 120_000);
    } catch (err) {
      toast(err instanceof Error ? err.message : String(err));
    }
  };
  const active = (): boolean => session.current !== null || rec.current !== null;
  // Leaving the chat drops a recording in progress.
  useEffect(() => () => {
    chat.current++;
    clearTimeout(limit.current);
    void session.current?.stop(false);
    session.current = null;
    void rec.current?.stop();
    rec.current = null;
  }, [props.agent.id]);

  return (
    <button
      type="button"
      class={`mic${recording ? " on" : ""}`}
      aria-label={recording ? "Stop dictating" : "Dictate a message"}
      title={recording ? "Stop: the text stays in the box" : "Hold to talk, or click to start and stop; the text goes into the box"}
      aria-pressed={recording}
      disabled={transcribing}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        if (active()) {
          void finish();
        } else {
          pressedAt.current = Date.now();
          void start();
        }
      }}
      onPointerUp={() => {
        if (active() && pressedAt.current && Date.now() - pressedAt.current > 400) void finish();
        pressedAt.current = 0;
      }}
      onClick={(e) => {
        // Keyboard presses arrive as clicks without pointer events.
        if (e.detail === 0) void (active() ? finish() : start());
      }}
    >
      {recording ? <Square size={16} /> : <Mic size={18} />}
    </button>
  );
}

function Composer(props: {
  agent: Agent;
  send: (body: string) => Promise<unknown>;
  /** `null`: voice is off, no microphone. */
  voice: VoiceView | null;
  onTalk: () => void;
}): JSX.Element {
  const { run } = useApp();
  const [text, setText] = useState("");
  const current = useRef(text);
  current.current = text;
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
    const ok = await run(() => props.send(body));
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
      {props.voice && (
        <MicButton agent={props.agent} voice={props.voice} text={() => current.current} setText={setText} onTalk={props.onTalk} />
      )}
      <button type="submit" class="send" disabled={!text.trim() || sending} aria-label="Send" title="Send (Enter)">
        <SendHorizontal size={18} />
      </button>
    </form>
  );
}
