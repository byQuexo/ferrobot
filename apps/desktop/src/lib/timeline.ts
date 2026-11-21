// Turns the daemon's messages and turn events into chat items. Pure, so the
// merge rules are unit-tested without a browser.
import type { MailMessage, QuestionAnswered, QuestionCard, StoredEvent } from "../api.ts";
import { credentialLabel } from "./format.ts";

export type TimelineItem =
  | { type: "user"; key: string; at: string; text: string; status: MailMessage["status"] }
  | { type: "incoming"; key: string; at: string; text: string; from: string; peerId: string }
  | { type: "routine"; key: string; at: string; text: string; routine: string | null }
  | { type: "outgoing"; key: string; at: string; text: string; to: string; peerId: string }
  | { type: "assistant"; key: string; at: string; text: string; streaming: boolean }
  | {
      type: "tool";
      key: string;
      at: string;
      name: string;
      input: unknown;
      output: string | null;
      isError: boolean;
      denied: string | null;
    }
  | {
      type: "question";
      key: string;
      at: string;
      id: number;
      card: QuestionCard;
      answered: QuestionAnswered | null;
    }
  | { type: "file"; key: string; at: string; id: number; name: string; size: number }
  | { type: "error"; key: string; at: string; text: string }
  | { type: "system"; key: string; at: string; text: string };

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

const SYSTEM_TEXT: Record<string, (p: Payload) => string> = {
  sleeping: (p) =>
    str(p["reason"]) === "idle" ? "Went to sleep after being idle" : "Went to sleep",
  awake: () => "Woke up",
  restored: (p) => `Computer restored from snapshot ${str(p["label"]) || ""}`.trim(),
  snapshot: (p) => `Snapshot taken${str(p["label"]) ? `: ${str(p["label"])}` : ""}`,
  sandbox_restarted: () => "Computer restarted after a problem",
  agent_created: (p) => `${str(p["by"])} created ${str(p["name"])} (auto mode)`,
  runner_refused: (p) => str(p["reason"]),
};

/**
 * An agent's chat without its call room messages and the turns they started (their events
 * and streaming text): those belong to the room. Cards and files have no turn and stay.
 */
export function withoutRoomTurns(
  messages: readonly MailMessage[],
  events: readonly StoredEvent[],
  live: ReadonlyMap<string, string>,
): {
  messages: MailMessage[];
  events: StoredEvent[];
  live: Map<string, string>;
} {
  const inRoom = new Set<string>();
  for (const e of events) {
    if (e.kind === "message_in" && e.turn_id && (e.payload as Payload)["room_id"] != null)
      inRoom.add(e.turn_id);
  }
  return {
    messages: messages.filter((m) => m.room_id == null),
    events: events.filter((e) => !e.turn_id || !inRoom.has(e.turn_id)),
    live: new Map([...live].filter(([turn]) => !inRoom.has(turn))),
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
  for (const e of events) {
    if (e.kind === "question_answered") {
      const a = e.payload as unknown as QuestionAnswered;
      answers.set(a.question, a);
    }
  }
  const answerMessages = new Set([...answers.values()].map((a) => a.message_id));

  for (const m of messages) {
    const key = `m${String(m.id)}`;
    if (answerMessages.has(m.id)) continue;
    if (m.recipient === agentId) {
      if (m.sender.kind === "user") {
        push({ type: "user", key, at: m.created_at, text: m.body, status: m.status });
      } else if (m.sender.kind === "agent") {
        push({
          type: "incoming",
          key,
          at: m.created_at,
          text: m.body,
          from: names.agents.get(m.sender.id) ?? "agent",
          peerId: m.sender.id,
        });
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
  // A turn that hit a credential problem shows the problem's notice instead of its error.
  const credentialTurns = new Set(
    events.flatMap((e) => {
      const turn = (e.payload as Payload)["turn_id"];
      return e.kind === "credential_error" && typeof turn === "string" ? [turn] : [];
    }),
  );
  for (const e of events) {
    const p = e.payload as unknown as Payload;
    const key = `e${String(e.id)}`;
    switch (e.kind) {
      case "assistant_text":
        if (e.turn_id) finished.add(e.turn_id);
        push({ type: "assistant", key, at: e.created_at, text: str(p["text"]), streaming: false });
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
      case "turn_done":
        if (p["outcome"] === "failed" && e.turn_id && credentialTurns.has(e.turn_id)) break;
        if (p["outcome"] === "failed") {
          push({
            type: "error",
            key,
            at: e.created_at,
            text: str(p["error"]) || "The turn failed",
          });
        } else if (p["outcome"] === "interrupted") {
          push({ type: "system", key, at: e.created_at, text: "Stopped" });
        }
        break;
      default: {
        const text = SYSTEM_TEXT[e.kind]?.(p);
        if (text) push({ type: "system", key, at: e.created_at, text });
      }
    }
  }

  const now = new Date().toISOString();
  for (const [turnId, text] of live) {
    if (!finished.has(turnId) && text) {
      push({ type: "assistant", key: `live-${turnId}`, at: now, text, streaming: true });
    }
  }

  items.sort((a, b) => {
    const t = Date.parse(a.item.at) - Date.parse(b.item.at);
    return t !== 0 ? t : a.order - b.order;
  });
  // A successful ask_user call is shown as its card instead of a tool card.
  const isCardCall = (i: TimelineItem): boolean =>
    i.type === "tool" && /(^|__)ask_user$/.test(i.name) && !i.isError;
  const shown = items.map((x) => x.item).filter((i) => !isCardCall(i));
  // Sleep deepens in steps (paused, then stopped); the chat shows the first one.
  return shown.filter((i, n) => {
    const prev = shown[n - 1];
    return !(i.type === "system" && prev?.type === "system" && prev.text === i.text);
  });
}

export type A2aItem = Extract<TimelineItem, { type: "incoming" | "outgoing" }>;
type ToolItem = Extract<TimelineItem, { type: "tool" }>;

/** A run of agent-to-agent mail with one peer, shown collapsed in the chat. */
export interface Exchange {
  type: "exchange";
  key: string;
  at: string;
  peerId: string;
  peerName: string;
  messages: A2aItem[];
  /** The `send_message` calls that produced the outgoing messages. */
  tools: ToolItem[];
}

export type DisplayItem = TimelineItem | Exchange;

const isSendCall = (i: TimelineItem): i is ToolItem =>
  i.type === "tool" && i.denied === null && /(^|__)send_message$/.test(i.name);

/**
 * Folds consecutive agent-to-agent mail with one peer, and the `send_message` calls right
 * before it, into one `exchange`; anything else in between (user mail, replies, tools) ends it.
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
      let run = cur.run;
      if (run?.peerId !== item.peerId) {
        close();
        const peerName = item.type === "incoming" ? item.from : item.to;
        run = {
          type: "exchange",
          key: `x-${item.key}`,
          at: item.at,
          peerId: item.peerId,
          peerName,
          messages: [],
          tools: [],
        };
        cur.run = run;
      }
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

/** Files shared in a chat by name (latest wins), so mentions can open them. */
export function sharedFiles(events: readonly StoredEvent[]): Map<string, number> {
  const out = new Map<string, number>();
  for (const e of events) {
    const name = (e.payload as Payload)["name"];
    if (e.kind === "file" && typeof name === "string") out.set(name, e.id);
  }
  return out;
}
