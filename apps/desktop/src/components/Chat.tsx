import type { JSX } from "preact";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "preact/hooks";
import {
  FileText,
  AlertTriangle,
  ArrowDownLeft,
  ArrowUpRight,
  CalendarClock,
  Camera,
  ChevronRight,
  CircleStop,
  Footprints,
  MessagesSquare,
  Info,
  Mic,
  Moon,
  Paperclip,
  Phone,
  PhoneOff,
  SendHorizontal,
  RotateCcw,
  ShieldAlert,
  SlidersHorizontal,
  Square,
  Sun,
  UserPlus,
  Wrench,
  X,
} from "lucide-preact";

import type { Agent, MailMessage, Routine, StoredEvent, Upload, VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import { isPass } from "../lib/call.ts";
import { clockTime, statusOf, stepLabel, stepsLabel, toolLabel } from "../lib/format.ts";
import {
  buildTimeline,
  firstNewKey,
  foldSteps,
  groupExchanges,
  sharedFiles,
  unreadSince,
  withoutRoomTurns,
  type DisplayItem,
  type Exchange,
  type TimelineItem,
} from "../lib/timeline.ts";
import { mentionQuery } from "../lib/mentions.ts";
import { hasVoiceKey, record, startLive, type LiveSession, type Recording } from "../lib/voice.ts";
import { AttachmentChips, previews } from "./Attachments.tsx";
import { FilePreview } from "./FilePreview.tsx";
import { fileSize } from "./FileViewer.tsx";
import { ContextMeter, ModelChip, TagChips } from "./AgentMeta.tsx";
import { Avatar } from "./Avatar.tsx";
import { CredentialFix } from "./CredentialFix.tsx";
import { MailGroup } from "./MailGroup.tsx";
import { QuestionCardView } from "./QuestionCard.tsx";
import { RichText, type ChatFiles } from "./RichText.tsx";
import { Button, IconButton } from "./ui.tsx";
import { VoiceQuick } from "./VoiceQuick.tsx";
import { VOICE_LABELS } from "./VoiceSettings.tsx";

export function Chat(props: {
  agent: Agent;
  infoOpen: boolean;
  onToggleInfo: () => void;
  onEdit: () => void;
  onVault: () => void;
  /** Set when the agent's model needs a ChatGPT sign-in rather than a key. */
  onSignIn?: () => void;
  /** Opens the details panel on Activities. */
  onActivities: () => void;
}): JSX.Element {
  const { client, agents, listen, run, call, callState } = useApp();
  const agent = props.agent;
  const id = agent.id;
  const [messages, setMessages] = useState<MailMessage[]>([]);
  const [events, setEvents] = useState<StoredEvent[]>([]);
  const [routines, setRoutines] = useState<Routine[]>([]);
  const [live, setLive] = useState<Map<string, string>>(new Map());
  const [loaded, setLoaded] = useState(false);
  // Where the "New" divider goes, fixed when the chat opens: reading it zeroes `unread`.
  const [newSince, setNewSince] = useState<number | null>(null);
  const [voiceView, setVoiceView] = useState<VoiceView | null>(null);
  const [quick, setQuick] = useState(false);
  useEffect(() => {
    client.voice().then(setVoiceView, () => undefined);
  }, [client]);
  const voiceEnabled = voiceView?.enabled !== false;
  const inCall = callState !== null && !callState.ending && callState.agents.includes(id);
  // A one-on-one call happens here; a call with a room is spoken to in the room.
  const send = (body: string): Promise<unknown> =>
    inCall && callState.room === null ? call.say(body) : client.send(id, body);

  useEffect(() => {
    let cancelled = false;
    setLoaded(false);
    setMessages([]);
    setEvents([]);
    setLive(new Map());
    setNewSince(null);
    const unread = agent.unread;
    const load = async (): Promise<void> => {
      const all = await client.allEvents(id);
      const [msgs, rts] = await Promise.all([client.messages(id), client.routines(id)]);
      if (cancelled) return;
      setMessages(msgs);
      setEvents(all);
      setRoutines(rts);
      const chat = withoutRoomTurns(msgs, all, new Map());
      setNewSince(unreadSince(chat.messages, chat.events, unread));
      setLoaded(true);
      void client.markRead(id).catch(() => undefined);
    };
    void run(load);
    const off = listen((e) => {
      switch (e.type) {
        case "event": {
          if (e.event.agent_id !== id) return;
          setEvents((prev) => (prev.some((x) => x.id === e.event.id) ? prev : [...prev, e.event]));
          if (
            e.event.turn_id &&
            (e.event.kind === "assistant_text" || e.event.kind === "turn_done")
          ) {
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
        case "delta":
          if (e.agent_id !== id) return;
          setLive((prev) => new Map(prev).set(e.turn_id, (prev.get(e.turn_id) ?? "") + e.text));
          break;
        case "message": {
          const m = e.message;
          const mine = m.recipient === id || (m.sender.kind === "agent" && m.sender.id === id);
          if (!mine) return;
          setMessages((prev) =>
            prev.some((x) => x.id === m.id)
              ? prev.map((x) => (x.id === m.id ? m : x))
              : [...prev, m],
          );
          break;
        }
        case "routines_changed":
          if (e.agent_id === id) void client.routines(id).then(setRoutines, () => undefined);
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
    const chat = withoutRoomTurns(messages, events, live);
    const all = buildTimeline(id, chat.messages, chat.events, chat.live, names);
    // Tool calls fold into "N steps" lines; their details and most turn errors live on
    // Activities. The running run shows as a short trail instead.
    const grouped = groupExchanges(all.filter((i) => i.type !== "error" || i.quiet === true));
    const { shown, live: running } = foldSteps(grouped, agent.presence === "working");
    const nameOf = (agentId: string): string | undefined => agents.get(agentId)?.name;
    const files: ChatFiles = { agent: id, byName: sharedFiles(events) };
    return {
      shown,
      files,
      roomTurn: chat.roomTurn,
      trail: running.slice(-3).map((t) => stepLabel(t.name, t.input, nameOf)),
    };
  }, [id, messages, events, live, agents, routines, agent.presence]);

  // New items rise in; the history that was there when the chat opened does not.
  const firstSeen = useRef(new Map<string, number>());
  const now = Date.now();
  for (const item of items.shown)
    if (!firstSeen.current.has(item.key)) firstSeen.current.set(item.key, loaded ? now : 0);
  const fresh = (key: string): boolean => now - (firstSeen.current.get(key) ?? 0) < 600;

  // Keep the view pinned to the bottom unless the user scrolled up.
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const pin = (): void => {
    const el = scroller.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  };
  useLayoutEffect(pin, [items.shown, items.trail]);
  const newKey = firstNewKey(items.shown, newSince);

  const working = agent.presence === "working";
  const status = statusOf(agent);
  const problem = agent.credential_error ?? agent.tools_unsupported;

  return (
    <section class="chat" aria-label={`Chat with ${agent.name}`}>
      <header class="chat-head">
        <Avatar agent={agent} size={40} presence={status.presence} />
        <div class="chat-title">
          <div class="chat-name">
            <h1>{agent.name}</h1>
            <TagChips tags={agent.tags} />
            <ModelChip agent={agent} />
          </div>
          <span class={`chat-sub p-${status.presence}`} title={problem?.reason}>
            <span class="dot" />
            {status.label}
            {problem ? (
              <CredentialFix
                agent={agent}
                onEdit={props.onEdit}
                onVault={props.onVault}
                onSignIn={props.onSignIn}
              />
            ) : (
              agent.description && <span class="muted"> · {agent.description}</span>
            )}
          </span>
        </div>
        <span class="spacer" />
        {working && (
          <Button variant="secondary" small onClick={() => void run(() => client.interrupt(id))}>
            <CircleStop size={16} /> Stop turn
          </Button>
        )}
        {voiceEnabled && (
          <span class="voice-tools">
            <IconButton
              label="Voice, language and speed"
              onClick={() => {
                setQuick(!quick);
              }}
              pressed={quick}
            >
              <SlidersHorizontal size={18} />
            </IconButton>
            {quick && voiceView && (
              <VoiceQuick
                agent={agent}
                view={voiceView}
                onView={setVoiceView}
                onClose={() => {
                  setQuick(false);
                }}
              />
            )}
            <IconButton
              label={
                inCall
                  ? "End the call"
                  : callState
                    ? "End the current call first"
                    : "Start a voice call"
              }
              onClick={() => {
                if (inCall) call.hangUp();
                else void call.start(id);
              }}
              pressed={inCall}
              disabled={!inCall && callState !== null}
            >
              {inCall ? <PhoneOff size={18} /> : <Phone size={18} />}
            </IconButton>
          </span>
        )}
        <IconButton
          label={props.infoOpen ? "Hide details" : "Show details"}
          onClick={props.onToggleInfo}
          pressed={props.infoOpen}
        >
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
        // Diagrams and frames grow the chat when they load.
        onLoadCapture={pin}
      >
        {loaded && items.shown.length === 0 && (
          <div class="chat-empty">
            <Avatar agent={agent} size={64} />
            <p class="empty-title">Say hello to {agent.name}</p>
            <p class="muted">
              Messages are delivered to the agent’s own computer. It wakes up automatically when
              something arrives.
            </p>
          </div>
        )}
        {items.shown.map((item) => (
          <div key={item.key} class={fresh(item.key) ? "enter" : "settled"}>
            {item.key === newKey && (
              <div class="row new-divider" role="separator">
                <span>New</span>
              </div>
            )}
            <Item item={item} agent={agent} files={items.files} onActivities={props.onActivities} />
          </div>
        ))}
        {working &&
          !items.roomTurn &&
          !items.shown.some((i) => i.type === "assistant" && i.streaming) && (
            <div class="row row-agent">
              <Avatar agent={agent} size={28} mood="working" />
              <div class="step-trail" role="status">
                {items.trail.slice(0, -1).map((step, n) => (
                  <span key={`${String(n)}-${step}`} class="step-old">
                    {step}
                  </span>
                ))}
                <span class="step-now">{items.trail.at(-1) ?? "Getting started"}…</span>
              </div>
            </div>
          )}
      </div>

      <Composer
        chat={id}
        placeholder={
          agent.presence === "sleeping"
            ? `Message ${agent.name} — this wakes it up`
            : `Message ${agent.name}`
        }
        send={send}
        voice={voiceEnabled ? voiceView : null}
        onTalk={() => {
          call.quiet();
        }}
        extra={<ContextMeter agent={agent} />}
        upload={(file) => client.upload(id, file)}
      />
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
        <button
          type="button"
          class="exchange-hint"
          aria-expanded={open}
          onClick={() => {
            setOpen(!open);
          }}
        >
          <span>
            {n} {n === 1 ? "message" : "messages"} with
          </span>
          {peer && <Avatar agent={peer} size={18} />}
          <strong>{peer?.name ?? item.peerName}</strong>
          <ChevronRight size={16} class="chev" />
        </button>
        {peer && (
          <button
            type="button"
            class="exchange-open"
            title={`Open the thread between ${agent.name} and ${peer.name}`}
            aria-label="Open thread"
            onClick={() => {
              openConversation(agent.id, peer.id);
            }}
          >
            <MessagesSquare size={16} />
          </button>
        )}
      </div>
      {open &&
        [...item.tools, ...item.messages]
          .sort((a, b) => Date.parse(a.at) - Date.parse(b.at))
          .map((m) => <Item key={m.key} item={m} agent={agent} />)}
    </>
  );
}

const NOTICE_ICONS: Record<string, typeof Moon> = {
  sleeping: Moon,
  awake: Sun,
  snapshot: Camera,
  restored: RotateCcw,
  sandbox_restarted: RotateCcw,
  agent_created: UserPlus,
  agent_tools_changed: Wrench,
  stopped: CircleStop,
};

function Item(props: {
  item: DisplayItem;
  agent: Agent;
  files?: ChatFiles;
  onActivities?: () => void;
}): JSX.Element {
  const { item, agent } = props;
  const { agents, openFile } = useApp();
  switch (item.type) {
    case "steps":
      return (
        <div class="row row-center">
          <button
            type="button"
            class="steps-line"
            title={item.tools
              .map((t) => stepLabel(t.name, t.input, (a) => agents.get(a)?.name))
              .join("\n")}
            onClick={props.onActivities}
          >
            <Footprints size={14} /> {stepsLabel(item.tools.length, item.seconds)}
          </button>
        </div>
      );
    case "exchange":
      return item.peers.length > 1 ? (
        <MailGroup item={item} />
      ) : (
        <ExchangeHint item={item} agent={agent} />
      );
    case "user":
      return (
        <>
          <div class="row row-user">
            <div class="bubble bubble-user">
              {item.files.length > 0 && <AttachmentChips agent={agent.id} files={item.files} />}
              {item.text && <RichText text={item.text} />}
              <span class="meta">
                {clockTime(item.at)}
                {item.status === "pending"
                  ? " · queued"
                  : item.status === "failed"
                    ? " · not delivered"
                    : ""}
              </span>
              {item.reaction && (
                <span class="reaction" title={`${agent.name} reacted`}>
                  {item.reaction}
                </span>
              )}
            </div>
          </div>
          {item.error && (
            <div class="row row-user">
              <p class="send-error">
                <AlertTriangle size={14} /> {item.error}
              </p>
            </div>
          )}
        </>
      );
    case "assistant":
      return (
        <div class="row row-agent">
          <div
            class={`bubble-agent${item.streaming ? " streaming" : ""}${isPass(item.text) ? " pass" : ""}`}
          >
            <RichText text={item.text} files={props.files} />
            {!item.streaming && <span class="meta">{clockTime(item.at)}</span>}
          </div>
        </div>
      );
    case "file":
      return (
        <div class="row row-agent file-share">
          <button
            type="button"
            class="file-card"
            onClick={() => {
              openFile(agent.id, item.id);
            }}
          >
            <FileText size={16} />
            <span class="file-name">{item.name}</span>
            <span class="meta">
              {fileSize(item.size)} · {clockTime(item.at)}
            </span>
          </button>
          <FilePreview agent={agent.id} file={item.id} name={item.name} />
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
              {item.type === "incoming" ? <ArrowDownLeft size={16} /> : <ArrowUpRight size={16} />}
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
              <CalendarClock size={16} />
              <span>
                {item.routine === null ? (
                  "A routine"
                ) : (
                  <>
                    Routine <strong>{item.routine}</strong>
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
    case "tool":
      return <ToolCard item={item} />;
    case "question":
      return (
        <QuestionCardView agent={agent} id={item.id} card={item.card} answered={item.answered} />
      );
    case "error":
      return (
        <div class="row row-center">
          <button type="button" class="turn-failed" title={item.text} onClick={props.onActivities}>
            <AlertTriangle size={14} /> Turn failed · {clockTime(item.at)} · details
          </button>
        </div>
      );
    case "system": {
      const Icon = NOTICE_ICONS[item.kind];
      return (
        <div class="row row-center">
          <div class="notice">
            {Icon && <Icon size={14} />}
            {item.text} · {clockTime(item.at)}
          </div>
        </div>
      );
    }
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

function prettyOutput(text: string): string {
  const t = text.trim();
  if (!t.startsWith("{") && !t.startsWith("[")) return text;
  try {
    return JSON.stringify(JSON.parse(t), null, 2);
  } catch {
    return text;
  }
}

export function ToolCard(props: { item: Extract<TimelineItem, { type: "tool" }> }): JSX.Element {
  const { item } = props;
  const { agents } = useApp();
  const [open, setOpen] = useState(false);
  const label = toolLabel(item.name);
  const headline = stepLabel(item.name, item.input, (id) => agents.get(id)?.name);
  const state =
    item.denied !== null
      ? "denied"
      : item.output === null
        ? "running"
        : item.isError
          ? "error"
          : "ok";
  return (
    <div class="row row-tool">
      <div class={`tool-card t-${state}${open ? " open" : ""}`}>
        <button
          type="button"
          class="tool-head"
          aria-expanded={open}
          onClick={() => {
            setOpen(!open);
          }}
        >
          <ChevronRight size={16} class="chev" />
          {state === "denied" ? <ShieldAlert size={16} /> : <Wrench size={16} />}
          <span class="tool-name">{headline}</span>
          {state === "denied" && <span class="tool-summary">Blocked: {item.denied ?? ""}</span>}
          <span class={`tool-state s-${state}`}>
            {state === "running"
              ? "Running"
              : state === "ok"
                ? "Done"
                : state === "error"
                  ? "Error"
                  : "Blocked"}
          </span>
        </button>
        {open && (
          <div class="tool-body">
            <div class="tool-label">
              {label.tool}
              {label.server && <span class="tool-server">{label.server}</span>}
            </div>
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

const joinText = (...parts: string[]): string =>
  parts
    .map((p) => p.trim())
    .filter(Boolean)
    .join(" ");

/**
 * Dictation: hold to talk, or click to start and stop. Words reach the message box live
 * (ElevenLabs) or after stopping (other providers); nothing is sent until the operator sends it.
 */
function MicButton(props: {
  chat: string;
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
    const provider = props.voice.provider;
    if (!hasVoiceKey(props.voice)) {
      toast(
        provider
          ? `Add your ${VOICE_LABELS[provider]} key in Vault to dictate`
          : "Choose a voice provider in Settings, Voice, to dictate",
      );
      return;
    }
    props.onTalk();
    const startedIn = chat.current;
    const before = props.text();
    try {
      if (props.voice.provider === "elevenlabs") {
        let said = "";
        const s = await startLive((rate) => client.voiceLive("dictation", rate), {
          partial: (p) => {
            props.setText(joinText(before, said, p));
          },
          committed: (c) => {
            said = joinText(said, c);
            props.setText(joinText(before, said));
          },
          error: (message) => {
            toast(`Dictation stopped: ${message}`);
            void finish();
          },
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
  useEffect(
    () => () => {
      chat.current++;
      clearTimeout(limit.current);
      void session.current?.stop(false);
      session.current = null;
      void rec.current?.stop();
      rec.current = null;
    },
    [props.chat],
  );

  return (
    <button
      type="button"
      class={`mic${recording ? " on" : ""}`}
      aria-label={recording ? "Stop dictating" : "Dictate a message"}
      title={
        recording
          ? "Stop: the text stays in the box"
          : "Hold to talk, or click to start and stop; the text goes into the box"
      }
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
      {recording ? <Square size={18} /> : <Mic size={18} />}
    </button>
  );
}

const MAX_UPLOAD = 25 * 1024 * 1024;

/** The message box of a chat or call room (`chat` is its id). */
export function Composer(props: {
  chat: string;
  placeholder: string;
  send: (body: string) => Promise<unknown>;
  /** `null`: voice is off, no microphone. */
  voice: VoiceView | null;
  onTalk: () => void;
  /** Shown before the buttons, e.g. the chat's context meter. */
  extra?: JSX.Element;
  /** Puts a file into the agent's workspace; without it nothing can be attached. */
  upload?: (file: File) => Promise<Upload>;
  /** Agents `@` offers to name; without them typing `@` opens nothing. */
  mentions?: Agent[];
}): JSX.Element {
  const { run } = useApp();
  const [text, setText] = useState("");
  const [pick, setPick] = useState<{ query: string; at: number; n: number } | null>(null);
  const options = pick
    ? (props.mentions ?? []).filter((a) => a.name.toLowerCase().startsWith(pick.query)).slice(0, 8)
    : [];
  const choose = (agent: Agent): void => {
    const el = ref.current;
    if (!el || !pick) return;
    const end = pick.at + agent.name.length + 2;
    setText(`${text.slice(0, pick.at)}@${agent.name} ${text.slice(el.selectionStart)}`);
    setPick(null);
    requestAnimationFrame(() => {
      el.setSelectionRange(end, end);
    });
  };
  const current = useRef(text);
  current.current = text;
  const [files, setFiles] = useState<File[]>([]);
  const [sending, setSending] = useState(false);
  const ref = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const canAttach = props.upload !== undefined;
  const attach = (list: ArrayLike<File>): void => {
    setFiles((prev) => [...prev, ...Array.from(list)]);
  };
  useEffect(() => {
    ref.current?.focus();
    setFiles([]);
  }, [props.chat]);
  useEffect(() => {
    if (!canAttach) return;
    const drop = (e: DragEvent): void => {
      attach(e.dataTransfer?.files ?? []);
    };
    window.addEventListener("drop", drop);
    return () => {
      window.removeEventListener("drop", drop);
    };
  }, [canAttach]);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${String(Math.min(el.scrollHeight, 200))}px`;
  }, [text]);

  const send = async (): Promise<void> => {
    const body = text.trim();
    if ((!body && files.length === 0) || sending) return;
    setSending(true);
    const ok = await run(async () => {
      const big = files.find((f) => f.size > MAX_UPLOAD);
      if (big) throw new Error(`${big.name} is larger than 25 MB`);
      const lines: string[] = [];
      for (const file of files) {
        const done = await props.upload?.(file);
        if (!done) continue;
        const image = file.type.startsWith("image/");
        if (image) previews.set(`${props.chat}/${done.path}`, URL.createObjectURL(file));
        lines.push(`Attached: ${done.path} (${image ? "image" : "file"}, ${fileSize(done.size)})`);
      }
      return props.send([body, ...lines].filter(Boolean).join("\n"));
    });
    setSending(false);
    if (ok) {
      setText("");
      setFiles([]);
    }
  };

  return (
    <form
      class="composer"
      onSubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      {pick && options.length > 0 && (
        <ul class="mention-pick" role="listbox" aria-label="Agents">
          {options.map((a, i) => (
            <li
              key={a.id}
              role="option"
              aria-selected={i === pick.n % options.length}
              onMouseDown={(e) => {
                e.preventDefault();
                choose(a);
              }}
            >
              <Avatar agent={a} size={20} />
              {a.name}
            </li>
          ))}
        </ul>
      )}
      {files.length > 0 && (
        <div class="attachments pending">
          {files.map((f, n) => (
            <span key={`${String(n)}-${f.name}`} class="attachment">
              <Paperclip size={14} />
              <span>{f.name}</span>
              <button
                type="button"
                aria-label={`Remove ${f.name}`}
                onClick={() => {
                  setFiles(files.filter((_, i) => i !== n));
                }}
              >
                <X size={14} />
              </button>
            </span>
          ))}
        </div>
      )}
      {canAttach && (
        <>
          <input
            ref={picker}
            type="file"
            multiple
            hidden
            onChange={(e) => {
              attach(e.currentTarget.files ?? []);
              e.currentTarget.value = "";
            }}
          />
          <button
            type="button"
            class="attach"
            aria-label="Attach files"
            title="Attach files (or drop or paste them)"
            onClick={() => picker.current?.click()}
          >
            <Paperclip size={18} />
          </button>
        </>
      )}
      <textarea
        ref={ref}
        rows={1}
        value={text}
        placeholder={props.placeholder}
        aria-expanded={options.length > 0}
        onInput={(e) => {
          const { value, selectionStart } = e.currentTarget;
          setText(value);
          if (props.mentions) {
            const found = mentionQuery(value, selectionStart);
            setPick(found && { ...found, n: 0 });
          }
        }}
        onPaste={(e) => {
          const images = Array.from(e.clipboardData?.files ?? []).filter((f) =>
            f.type.startsWith("image/"),
          );
          if (!canAttach || images.length === 0) return;
          e.preventDefault();
          attach(images);
        }}
        onKeyDown={(e) => {
          const chosen = pick && options[pick.n % options.length];
          if (pick && chosen && !e.isComposing) {
            const step = { ArrowDown: 1, ArrowUp: options.length - 1 }[e.key];
            if (step !== undefined) setPick({ ...pick, n: (pick.n + step) % options.length });
            else if (e.key === "Enter" || e.key === "Tab") choose(chosen);
            else if (e.key === "Escape") setPick(null);
            else return;
            e.preventDefault();
            return;
          }
          if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
            e.preventDefault();
            void send();
          }
        }}
      />
      {props.extra}
      {props.voice && (
        <MicButton
          chat={props.chat}
          voice={props.voice}
          text={() => current.current}
          setText={setText}
          onTalk={props.onTalk}
        />
      )}
      <button
        type="submit"
        class="send"
        disabled={(!text.trim() && files.length === 0) || sending}
        aria-label="Send"
        title="Send (Enter)"
      >
        <SendHorizontal size={18} />
      </button>
    </form>
  );
}
