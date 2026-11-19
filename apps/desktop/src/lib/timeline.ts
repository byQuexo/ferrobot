// Turns the daemon's messages and turn events into chat items. Pure, so the
// merge rules are unit-tested without a browser.
import type { MailMessage, StoredEvent } from "../api.ts";

export type TimelineItem =
  | { type: "user"; key: string; at: string; text: string; status: MailMessage["status"] }
  | { type: "incoming"; key: string; at: string; text: string; from: string }
  | { type: "routine"; key: string; at: string; text: string; routine: string }
  | { type: "outgoing"; key: string; at: string; text: string; to: string }
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
  sleeping: (p) => (str(p["reason"]) === "idle" ? "Went to sleep after being idle" : "Went to sleep"),
  awake: () => "Woke up",
  restored: (p) => `Computer restored from snapshot ${str(p["label"]) || ""}`.trim(),
  snapshot: (p) => `Snapshot taken${str(p["label"]) ? `: ${str(p["label"])}` : ""}`,
  sandbox_restarted: () => "Computer restarted after a problem",
};

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

  for (const m of messages) {
    const key = `m${String(m.id)}`;
    if (m.recipient === agentId) {
      if (m.sender.kind === "user") {
        push({ type: "user", key, at: m.created_at, text: m.body, status: m.status });
      } else if (m.sender.kind === "agent") {
        push({ type: "incoming", key, at: m.created_at, text: m.body, from: names.agents.get(m.sender.id) ?? "agent" });
      } else {
        const routine = names.routines.get(m.sender.id) ?? `#${String(m.sender.id)}`;
        push({ type: "routine", key, at: m.created_at, text: m.body, routine });
      }
    } else if (m.sender.kind === "agent" && m.sender.id === agentId) {
      push({ type: "outgoing", key, at: m.created_at, text: m.body, to: names.agents.get(m.recipient) ?? "agent" });
    }
  }

  const tools = new Map<string, Extract<TimelineItem, { type: "tool" }>>();
  const finished = new Set<string>();
  for (const e of events) {
    const p = e.payload as unknown as Payload;
    const key = `e${String(e.id)}`;
    switch (e.kind) {
      case "assistant_text":
        if (e.turn_id) finished.add(e.turn_id);
        push({ type: "assistant", key, at: e.created_at, text: str(p["text"]), streaming: false });
        break;
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
      case "turn_done":
        if (p["outcome"] === "failed") {
          push({ type: "error", key, at: e.created_at, text: str(p["error"]) || "The turn failed" });
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
  return items.map((x) => x.item);
}
