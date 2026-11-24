// Turns the daemon's messages and turn events into chat items. Pure, so the
// merge rules are unit-tested without a browser.
import type { MailMessage, QuestionAnswered, QuestionCard, StoredEvent } from "../api.ts";
import { TOOLS_UNSUPPORTED, credentialLabel } from "./format.ts";

export type TimelineItem =
  | {
      type: "user";
      key: string;
      at: string;
      text: string;
      files: Attachment[];
      /** The agent's emoji on this message. */
      reaction: string | null;
      status: MailMessage["status"];
      /** Why the last attempt failed, on a message that was not delivered. */
      error?: string;
    }
  | { type: "incoming"; key: string; at: string; text: string; from: string; peerId: string }
  | { type: "routine"; key: string; at: string; text: string; routine: string | null }
  /** From an incoming webhook; the text opens with the webhook's name. */
  | { type: "webhook"; key: string; at: string; text: string }
  | { type: "outgoing"; key: string; at: string; text: string; to: string; peerId: string }
  | {
      type: "assistant";
      key: string;
      at: string;
      text: string;
      streaming: boolean;
      /** Its `assistant_text` event; `null` while it streams. */
      id: number | null;
      /** The turn that wrote it. */
      turn: string | null;
      /** The operator's emoji on this message. */
      reaction: string | null;
    }
  | {
      type: "tool";
      key: string;
      at: string;
      name: string;
      input: unknown;
      output: string | null;
      isError: boolean;
      denied: string | null;
      /** The turn that made the call. */
      turn: string | null;
    }
  | {
      type: "question";
      key: string;
      at: string;
      id: number;
      card: QuestionCard;
      answered: QuestionAnswered | null;
    }
  | {
      type: "file";
      key: string;
      at: string;
      id: number;
      name: string;
      size: number;
      /** Where it was in the agent's computer, e.g. `/home/agent/plan.html`. */
      path: string;
    }
  | {
      type: "error";
      key: string;
      at: string;
      text: string;
      /** A failed turn the chat shows as one quiet line: no undelivered bubble says it already. */
      quiet?: boolean;
    }
  | { type: "system"; key: string; at: string; text: string; kind: string };

export interface Names {
  /** Agent id -> name. */
  agents: Map<string, string>;
  /** Routine id -> name. */
  routines: Map<number, string>;
}

type Payload = Record<string, unknown>;

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

/** A file the operator attached, from a line like `Attached: uploads/photo.png (image, 120 KB)`. */
export interface Attachment {
  path: string;
  kind: string;
  size: string;
}

const ATTACHED = /^Attached: (\S+) \(([^,()]+), ([^()]+)\)$/;

/** A message's text without its attachment lines, and the files they name. */
export function splitAttachments(body: string): { text: string; files: Attachment[] } {
  const files: Attachment[] = [];
  const rest = body.split("\n").filter((line) => {
    const m = ATTACHED.exec(line.trim());
    if (m) files.push({ path: m[1] ?? "", kind: m[2] ?? "", size: m[3] ?? "" });
    return !m;
  });
  return { text: rest.join("\n").trim(), files };
}

const SYSTEM_TEXT: Record<string, (p: Payload) => string> = {
  sleeping: (p) =>
    str(p["reason"]) === "idle" ? "Went to sleep after being idle" : "Went to sleep",
  awake: () => "Woke up",
  restored: (p) => `Computer restored from snapshot ${str(p["label"]) || ""}`.trim(),
  snapshot: (p) => `Snapshot taken${str(p["label"]) ? `: ${str(p["label"])}` : ""}`,
  sandbox_restarted: () => "Computer restarted after a problem",
  agent_created: (p) => `${str(p["by"])} created ${str(p["name"])} (auto mode)`,
  agent_tools_changed: (p) => `${str(p["by"])} changed the tools of ${str(p["name"])} (auto mode)`,
  runner_refused: (p) => str(p["reason"]),
  new_session: () => "New chat: the agent starts fresh from here",
  compacted: () => "Compacted the conversation: the agent keeps a summary of what came before",
  renamed: (p) => `Renamed from @${str(p["from"])} to @${str(p["to"])}`,
};

/**
 * An agent's chat without its call room messages and the turns they started (their events
 * and streaming text): those belong to the room. Cards and files have no turn and stay.
 * `roomTurn` says whether the newest turn is one of them.
 */
export function withoutRoomTurns(
  messages: readonly MailMessage[],
  events: readonly StoredEvent[],
  live: ReadonlyMap<string, string>,
): {
  messages: MailMessage[];
  events: StoredEvent[];
  live: Map<string, string>;
  roomTurn: boolean;
} {
  const inRoom = new Set<string>();
  let newest = "";
  for (const e of events) {
    if (e.turn_id) newest = e.turn_id;
    if (e.kind === "message_in" && e.turn_id && (e.payload as Payload)["room_id"] != null)
      inRoom.add(e.turn_id);
  }
  return {
    messages: messages.filter((m) => m.room_id == null),
    events: events.filter((e) => !e.turn_id || !inRoom.has(e.turn_id)),
    live: new Map([...live].filter(([turn]) => !inRoom.has(turn))),
    roomTurn: inRoom.has(newest),
  };
}

/**
 * Builds the chat for `agentId`. `live` holds streamed text of turns whose
 * final `assistant_text` has not arrived yet.
 */
export function buildTimeline(
  agentId: string,
  messages: readonly MailMessage[],
  events: readonly StoredEvent[],
  live: ReadonlyMap<string, string>,
  names: Names,
): TimelineItem[] {
  const items: { item: TimelineItem; order: number }[] = [];
  let order = 0;
  const push = (item: TimelineItem): void => {
    items.push({ item, order: order++ });
  };

  // Answers to question cards are shown on the card, not as a second bubble.
  const answers = new Map<number, QuestionAnswered>();
  // An agent's reaction shows on the operator's message; a newer one replaces it.
  const reactions = new Map<number, string>();
  // The operator's shows on the agent's message; the note that told the agent stays hidden.
  const mine = new Map<number, string>();
  const notes = new Set<number>();
  for (const e of events) {
    const p = e.payload as Payload;
    if (e.kind === "question_answered") {
      const a = e.payload as unknown as QuestionAnswered;
      answers.set(a.question, a);
    } else if (e.kind === "reaction" && typeof p["message_id"] === "number") {
      reactions.set(p["message_id"], str(p["emoji"]));
    } else if (e.kind === "user_reaction" && typeof p["event_id"] === "number") {
      mine.set(p["event_id"], str(p["emoji"]));
      if (typeof p["note_id"] === "number") notes.add(p["note_id"]);
    }
  }
  const answerMessages = new Set([...answers.values()].map((a) => a.message_id));

  // Turn errors stay on the Activities tab, except the last one of a message that failed.
  const turnMessage = new Map<string, number>();
  const failures = new Map<number, string>();
  const lastFailedTurn = new Map<number, string>();
  for (const e of events) {
    const p = e.payload as Payload;
    if (e.kind === "message_in" && e.turn_id && typeof p["message_id"] === "number")
      turnMessage.set(e.turn_id, p["message_id"]);
    const message = e.turn_id ? turnMessage.get(e.turn_id) : undefined;
    if (e.kind === "turn_done" && p["outcome"] === "failed" && message !== undefined) {
      failures.set(message, str(p["error"]) || "The turn failed");
      if (e.turn_id) lastFailedTurn.set(message, e.turn_id);
    }
  }
  const fromUser = new Set(
    messages.filter((m) => m.sender.kind === "user" && m.recipient === agentId).map((m) => m.id),
  );
  // Retries of the same message fail alike: only the last one gets a line.
  const quietFailure = (turn: string | null): boolean => {
    const message = turn ? turnMessage.get(turn) : undefined;
    if (message === undefined) return true;
    return !fromUser.has(message) && lastFailedTurn.get(message) === turn;
  };

  for (const m of messages) {
    const key = `m${String(m.id)}`;
    if (answerMessages.has(m.id) || notes.has(m.id)) continue;
    if (m.recipient === agentId) {
      if (m.sender.kind === "user") {
        const error = m.status === "failed" ? failures.get(m.id) : undefined;
        const { text, files } = splitAttachments(m.body);
        const reaction = reactions.get(m.id) ?? null;
        push({
          type: "user",
          key,
          at: m.created_at,
          text,
          files,
          reaction,
          status: m.status,
          error,
        });
      } else if (m.sender.kind === "agent") {
        push({
          type: "incoming",
          key,
          at: m.created_at,
          text: m.body,
          from: names.agents.get(m.sender.id) ?? "agent",
          peerId: m.sender.id,
        });
      } else if (m.sender.kind === "webhook") {
        push({ type: "webhook", key, at: m.created_at, text: m.body });
      } else {
        const routine = names.routines.get(m.sender.id) ?? null;
        push({ type: "routine", key, at: m.created_at, text: m.body, routine });
      }
    } else if (m.sender.kind === "agent" && m.sender.id === agentId) {
      push({
        type: "outgoing",
        key,
        at: m.created_at,
        text: m.body,
        to: names.agents.get(m.recipient) ?? "agent",
        peerId: m.recipient,
      });
    }
  }

  const tools = new Map<string, Extract<TimelineItem, { type: "tool" }>>();
  const finished = new Set<string>();
  // A turn that hit a credential or tools problem shows the problem's notice instead of its
  // error, which the Claude SDK also writes as the turn's answer.
  const credentialTurns = new Map(
    events.flatMap((e): [string, number][] => {
      const turn = (e.payload as Payload)["turn_id"];
      const problem = e.kind === "credential_error" || e.kind === "tools_unsupported";
      return problem && typeof turn === "string" ? [[turn, e.id]] : [];
    }),
  );
  for (const e of events) {
    const p = e.payload as unknown as Payload;
    const key = `e${String(e.id)}`;
    const afterCredentialError = e.turn_id
      ? e.id > (credentialTurns.get(e.turn_id) ?? e.id)
      : false;
    switch (e.kind) {
      case "assistant_text":
        if (e.turn_id) finished.add(e.turn_id);
        if (afterCredentialError) break;
        push({
          type: "assistant",
          key,
          at: e.created_at,
          text: str(p["text"]),
          streaming: false,
          id: e.id,
          turn: e.turn_id ?? null,
          reaction: mine.get(e.id) ?? null,
        });
        break;
      case "question": {
        const card = (p["card"] ?? null) as QuestionCard | null;
        if (card)
          push({
            type: "question",
            key,
            at: e.created_at,
            id: e.id,
            card,
            answered: answers.get(e.id) ?? null,
          });
        break;
      }
      case "tool_use": {
        const item = {
          type: "tool" as const,
          key,
          at: e.created_at,
          name: str(p["name"]),
          input: p["input"],
          output: null,
          isError: false,
          denied: null,
          turn: e.turn_id ?? null,
        };
        const id = str(p["id"]);
        if (id) tools.set(id, item);
        push(item);
        break;
      }
      case "tool_result": {
        const call = tools.get(str(p["id"]));
        if (call) {
          call.output = str(p["output"]);
          call.isError = p["is_error"] === true;
        }
        break;
      }
      case "file":
        push({
          type: "file",
          key,
          at: e.created_at,
          id: e.id,
          name: str(p["name"]),
          size: Number(p["size"] ?? 0),
          path: str(p["path"]),
        });
        break;
      case "tool_denied":
        push({
          type: "tool",
          key,
          at: e.created_at,
          name: str(p["name"]),
          input: null,
          output: null,
          isError: true,
          denied: str(p["reason"]),
          turn: e.turn_id ?? null,
        });
        break;
      case "credential_error":
        push({
          type: "error",
          key,
          at: e.created_at,
          text: `${credentialLabel(p["kind"] === "missing" ? "missing" : "rejected")}: ${str(p["reason"])}. Messages wait until it is fixed.`,
        });
        break;
      case "tools_unsupported":
        push({
          type: "error",
          key,
          at: e.created_at,
          text: `${TOOLS_UNSUPPORTED}: ${str(p["reason"])}. Messages wait until you choose.`,
        });
        break;
      case "turn_done":
        if (p["outcome"] === "failed" && afterCredentialError) break;
        if (p["outcome"] === "failed") {
          push({
            type: "error",
            key,
            at: e.created_at,
            text: str(p["error"]) || "The turn failed",
            quiet: quietFailure(e.turn_id ?? null),
          });
        } else if (p["outcome"] === "interrupted") {
          push({ type: "system", key, at: e.created_at, text: "Stopped", kind: "stopped" });
        }
        break;
      default: {
        const text = SYSTEM_TEXT[e.kind]?.(p);
        if (text) push({ type: "system", key, at: e.created_at, text, kind: e.kind });
      }
    }
  }

  const now = new Date().toISOString();
  for (const [turnId, text] of live) {
    if (!finished.has(turnId) && text) {
      push({
        type: "assistant",
        key: `live-${turnId}`,
        at: now,
        text,
        streaming: true,
        id: null,
        turn: turnId,
        reaction: null,
      });
    }
  }

  items.sort((a, b) => {
    const t = Date.parse(a.item.at) - Date.parse(b.item.at);
    return t !== 0 ? t : a.order - b.order;
  });
  // A successful ask_user call shows as its card and a react call as its emoji, not as steps.
  const isShownElsewhere = (i: TimelineItem): boolean =>
    i.type === "tool" && /(^|__)(ask_user|react)$/.test(i.name) && !i.isError;
  const shown = items.map((x) => x.item).filter((i) => !isShownElsewhere(i));
  // Sleep deepens in steps (paused, then stopped); the chat shows the first one.
  return shown.filter((i, n) => {
    const prev = shown[n - 1];
    return !(i.type === "system" && prev?.type === "system" && prev.text === i.text);
  });
}

export type A2aItem = Extract<TimelineItem, { type: "incoming" | "outgoing" }>;
type ToolItem = Extract<TimelineItem, { type: "tool" }>;

/** A run of agent-to-agent mail, shown collapsed in the chat. */
export interface Exchange {
  type: "exchange";
  key: string;
  at: string;
  /** The first peer. */
  peerId: string;
  peerName: string;
  /** Every peer's id, in order of first mail; more than one makes a multi-agent run. */
  peers: string[];
  messages: A2aItem[];
  /** The `send_message` calls that produced the outgoing messages. */
  tools: ToolItem[];
}

/** A finished run of tool calls, shown in the chat as one line that opens Activities. */
export interface Steps {
  type: "steps";
  key: string;
  at: string;
  tools: ToolItem[];
  seconds: number;
}

export type DisplayItem = TimelineItem | Exchange | Steps;

const isSendCall = (i: TimelineItem): i is ToolItem =>
  i.type === "tool" && i.denied === null && /(^|__)send_message$/.test(i.name);

/**
 * Folds consecutive agent-to-agent mail, with one peer or several, and the `send_message` calls
 * right before it, into one `exchange`; anything else in between (user mail, replies, tools) ends it.
 */
export function groupExchanges(items: readonly TimelineItem[]): DisplayItem[] {
  const out: DisplayItem[] = [];
  const cur: { run: Exchange | null } = { run: null };
  let calls: ToolItem[] = [];
  const close = (): void => {
    if (cur.run) out.push(cur.run);
    cur.run = null;
  };
  for (const item of items) {
    if (isSendCall(item)) {
      calls.push(item);
      continue;
    }
    if (item.type === "incoming" || item.type === "outgoing") {
      const run = (cur.run ??= {
        type: "exchange",
        key: `x-${item.key}`,
        at: item.at,
        peerId: item.peerId,
        peerName: item.type === "incoming" ? item.from : item.to,
        peers: [],
        messages: [],
        tools: [],
      });
      if (!run.peers.includes(item.peerId)) run.peers.push(item.peerId);
      run.tools.push(...calls);
      run.messages.push(item);
      calls = [];
      continue;
    }
    close();
    out.push(...calls, item);
    calls = [];
  }
  close();
  out.push(...calls);
  return out;
}

/** A row of an expanded multi-agent exchange. */
export interface ExchangeRow {
  key: string;
  at: string;
  type: A2aItem["type"];
  text: string;
  peers: { id: string; name: string }[];
}

/** One row per message; the same text sent to several agents in a row is one broadcast row. */
export function exchangeRows(messages: readonly A2aItem[]): ExchangeRow[] {
  const rows: ExchangeRow[] = [];
  for (const m of messages) {
    const peer = { id: m.peerId, name: m.type === "incoming" ? m.from : m.to };
    const last = rows[rows.length - 1];
    if (
      m.type === "outgoing" &&
      last?.type === "outgoing" &&
      last.text === m.text &&
      !last.peers.some((p) => p.id === m.peerId)
    )
      last.peers.push(peer);
    else rows.push({ key: m.key, at: m.at, type: m.type, text: m.text, peers: [peer] });
  }
  return rows;
}

/**
 * When the unread part of a chat starts: the first event of the oldest of the last `unread`
 * finished turns, or the mail that started that turn if it is older. `null`: nothing unread.
 */
export function unreadSince(
  messages: readonly MailMessage[],
  events: readonly StoredEvent[],
  unread: number,
): number | null {
  if (unread <= 0) return null;
  const done = events.filter((e) => e.kind === "turn_done" && e.turn_id);
  const turn = (done[done.length - unread] ?? done[0])?.turn_id;
  if (!turn) return null;
  let since = Infinity;
  for (const e of events) {
    if (e.turn_id !== turn) continue;
    since = Math.min(since, Date.parse(e.created_at));
    const id = (e.payload as Payload)["message_id"];
    const mail = e.kind === "message_in" ? messages.find((m) => m.id === id) : undefined;
    if (mail) since = Math.min(since, Date.parse(mail.created_at));
  }
  return since;
}

/** The key of the first item since `since` that is not the operator's own message. */
export function firstNewKey(items: readonly DisplayItem[], since: number | null): string | null {
  if (since === null) return null;
  return items.find((i) => i.type !== "user" && Date.parse(i.at) >= since)?.key ?? null;
}

/** Files shared in a chat by name (latest wins), so mentions can open them. */
export function sharedFiles(events: readonly StoredEvent[]): Map<string, number> {
  const out = new Map<string, number>();
  for (const e of events) {
    const name = (e.payload as Payload)["name"];
    if (e.kind === "file" && typeof name === "string") out.set(name, e.id);
  }
  return out;
}

/**
 * Folds each run of tool calls into one `steps` line. While the agent works, the run at the
 * end is still going: it comes back as `live` for the chat's step trail instead.
 */
export function foldSteps(
  items: readonly DisplayItem[],
  working: boolean,
): { shown: DisplayItem[]; live: ToolItem[] } {
  const shown: DisplayItem[] = [];
  let run: ToolItem[] = [];
  const close = (): void => {
    const [first, last] = [run[0], run[run.length - 1]];
    if (first && last) {
      const seconds = (Date.parse(last.at) - Date.parse(first.at)) / 1000;
      shown.push({ type: "steps", key: `s-${first.key}`, at: first.at, tools: run, seconds });
    }
    run = [];
  };
  for (const item of items) {
    if (item.type === "tool") {
      run.push(item);
      continue;
    }
    close();
    shown.push(item);
  }
  if (working) return { shown, live: run };
  close();
  return { shown, live: [] };
}
