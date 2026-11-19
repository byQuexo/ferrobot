import type { Presence } from "../api.ts";

export function presenceLabel(p: Presence): string {
  switch (p) {
    case "working":
      return "Working";
    case "awake":
      return "Awake";
    case "starting":
      return "Starting";
    case "sleeping":
      return "Sleeping";
    case "error":
      return "Needs attention";
  }
}

/** "just now", "5 min ago", "3 h ago", "2 d ago"; future times as "in 5 min". */
export function relativeTime(iso: string, now: number = Date.now()): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const diff = Math.round((now - t) / 1000);
  const abs = Math.abs(diff);
  let text: string;
  if (abs < 45) return diff >= 0 ? "just now" : "in a moment";
  else if (abs < 3600) text = `${String(Math.round(abs / 60))} min`;
  else if (abs < 86_400) text = `${String(Math.round(abs / 3600))} h`;
  else text = `${String(Math.round(abs / 86_400))} d`;
  return diff >= 0 ? `${text} ago` : `in ${text}`;
}

export function clockTime(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function dateTime(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime())
    ? ""
    : d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

/** Display name of a tool, without the MCP server prefix. */
export function toolLabel(name: string): { server: string | null; tool: string } {
  const m = /^mcp__([^_]+(?:_[^_]+)*)__(.+)$/.exec(name);
  return m?.[1] && m[2] ? { server: m[1], tool: m[2] } : { server: null, tool: name };
}

export function duration(seconds: number): string {
  if (seconds < 60) return `${String(seconds)} s`;
  if (seconds < 3600) return `${String(Math.floor(seconds / 60))} min`;
  return `${String(Math.floor(seconds / 3600))} h ${String(Math.floor((seconds % 3600) / 60))} min`;
}
