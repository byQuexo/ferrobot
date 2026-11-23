// A turn's trace laid out as a timeline: where each step's bar starts and how wide it is.
// Pure, so the layout rules are unit-tested without a browser.
import type { Trace } from "../api.ts";
import { stepLabel } from "./format.ts";

export interface TraceRow {
  index: number;
  kind: "model" | "tool";
  name: string;
  input: unknown;
  error: string | null;
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

export function layoutTrace(trace: Trace, now: number = Date.now()): TraceLayout {
  const start = Date.parse(trace.started_at);
  const running = trace.ended_at == null;
  const end = running ? now : Date.parse(trace.ended_at ?? "");
  const total = Math.max(end - start, 1);
  const rows = trace.spans.map((s, index): TraceRow => {
    const from = Date.parse(s.started_at);
    const to = s.ended_at == null ? end : Date.parse(s.ended_at);
    const left = Math.min(Math.max(((from - start) / total) * 100, 0), 100);
    const width = Math.min(Math.max(((to - from) / total) * 100, MIN_WIDTH), 100 - left);
    return {
      index,
      kind: s.kind,
      name: s.name,
      input: s.input ?? null,
      error: s.error ?? null,
      left,
      width,
      ms: Math.max(to - from, 0),
      running: s.ended_at == null,
      slowest: false,
      labelAt:
        left + width <= LABEL_ROOM ? "after" : left >= 100 - LABEL_ROOM ? "before" : "inside",
    };
  });
  const longest = rows.reduce<TraceRow | null>((a, r) => (!a || r.ms > a.ms ? r : a), null);
  if (longest && rows.length > 1 && longest.ms > 0) longest.slowest = true;
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

/** What a step did: the model, the command or file of a computer tool, else the chat's label. */
export function traceLabel(
  kind: "model" | "tool",
  name: string,
  input: unknown,
  nameOf: (id: string) => string | undefined,
): string {
  if (kind === "model") return name;
  const tool = name.split("__").pop() ?? name;
  const arg = (key: string): string | null => {
    const v = (input as Record<string, unknown> | null)?.[key];
    return typeof v === "string" && v ? v : null;
  };
  const cmd = arg("command");
  if (tool === "bash" && cmd) return `$ ${cmd.split("\n")[0] ?? cmd}`;
  const path = arg("path");
  if (tool === "read_file" && path) return `Reading ${path}`;
  if (tool === "write_file" && path) return `Writing ${path}`;
  return stepLabel(name, input, nameOf);
}
