// A turn's trace laid out as a timeline: where each step's bar starts and how wide it is.
// Pure, so the layout rules are unit-tested without a browser.
import type { Trace, TurnLink } from "../api.ts";
import { stepLabel } from "./format.ts";

export type SpanKind = Trace["spans"][number]["kind"];

export interface TraceRow {
  index: number;
  kind: SpanKind;
  name: string;
  input: unknown;
  error: string | null;
  /** A wait's outcome; `null` while open. */
  outcome: string | null;
  /** A handoff's recipient and the turn that took it. */
  link: TurnLink | null;
  /** Percent of the turn's length. */
  left: number;
  width: number;
  ms: number;
  running: boolean;
  slowest: boolean;
  /** Where the duration goes: after the bar, before it near the right edge, or inside a bar
   * that spans nearly everything. */
  labelAt: "after" | "before" | "inside";
}

/** A bar ending past this percent has no room for its label after it. */
const LABEL_ROOM = 82;

export interface TraceLayout {
  /** The turn's length so far. */
  ms: number;
  running: boolean;
  rows: TraceRow[];
  models: number;
  tools: number;
  errors: number;
}

/** Bars narrower than this would vanish. */
const MIN_WIDTH = 0.6;
/** A step only counts as the slowest when it took at least this long. */
const SLOW_MS = 1000;

export function layoutTrace(trace: Trace, now: number = Date.now()): TraceLayout {
  const start = Date.parse(trace.started_at);
  const running = trace.ended_at == null;
  const end = running ? now : Date.parse(trace.ended_at ?? "");
  const total = Math.max(end - start, 1);
  const rows = trace.spans.map((s, index): TraceRow => {
    const from = Date.parse(s.started_at);
    // An open wait runs on with the turn; an answer after the turn's end still counts in full.
    const to = s.ended_at == null ? end : Date.parse(s.ended_at);
    // A task's answer goes out just after the turn's end; it still gets a visible mark.
    const left = Math.min(Math.max(((from - start) / total) * 100, 0), 100 - MIN_WIDTH);
    const width = Math.min(Math.max(((to - from) / total) * 100, MIN_WIDTH), 100 - left);
    return {
      index,
      kind: s.kind,
      name: s.name,
      input: s.input ?? null,
      error: s.error ?? null,
      outcome: s.outcome ?? null,
      link: s.link ?? null,
      left,
      width,
      ms: Math.max(to - from, 0),
      running: s.ended_at == null,
      slowest: false,
      labelAt:
        left + width <= LABEL_ROOM ? "after" : left >= 100 - LABEL_ROOM ? "before" : "inside",
    };
  });
  // Waiting on the operator is not work, so it never counts as the slowest step.
  const steps = rows.filter((r) => r.kind === "model" || r.kind === "tool");
  const longest = steps.reduce<TraceRow | null>((a, r) => (!a || r.ms > a.ms ? r : a), null);
  if (longest && steps.length > 1 && longest.ms >= SLOW_MS) longest.slowest = true;
  return {
    ms: end - start,
    running,
    rows,
    models: rows.filter((r) => r.kind === "model").length,
    tools: rows.filter((r) => r.kind === "tool").length,
    errors: rows.filter((r) => r.error !== null).length + (trace.outcome === "failed" ? 1 : 0),
  };
}

/** "0.4s", "9.9s", "12s", "1m 5s". */
export function traceDuration(ms: number): string {
  if (ms < 10_000) return `${(ms / 1000).toFixed(1)}s`;
  const s = Math.round(ms / 1000);
  return s < 60 ? `${String(s)}s` : `${String(Math.floor(s / 60))}m ${String(s % 60)}s`;
}

/** What a step did: the model, the command or file of a computer tool, what a wait asked,
 * who a handoff went to, else the chat's label. */
export function traceLabel(
  kind: SpanKind,
  name: string,
  input: unknown,
  nameOf: (id: string) => string | undefined,
  link: TurnLink | null = null,
): string {
  if (kind === "model") return name;
  const arg = (key: string): string | null => {
    const v = (input as Record<string, unknown> | null)?.[key];
    return typeof v === "string" && v ? v : null;
  };
  if (kind === "wait") return arg("title") ?? (name === "ask_user" ? "Question" : "Approval");
  if (kind === "handoff") {
    const other = link && (nameOf(link.agent_id) ?? link.agent_name);
    const who = other ? `@${other}` : "a teammate";
    if (name === "task") return `Task for ${who}`;
    return name === "answer" ? `Answer to ${who}` : `Message to ${who}`;
  }
  const tool = name.split("__").pop() ?? name;
  const cmd = arg("command");
  if (tool === "bash" && cmd) return `$ ${cmd.split("\n")[0] ?? cmd}`;
  const path = arg("path");
  if (tool === "read_file" && path) return `Reading ${path}`;
  if (tool === "write_file" && path) return `Writing ${path}`;
  return stepLabel(name, input, nameOf);
}

/** How a wait ended: its outcome, else still waiting or left without an answer. */
export function waitStatus(outcome: string | null, running: boolean): string {
  if (outcome) return outcome.replace("_", " ");
  return running ? "waiting" : "no answer";
}
