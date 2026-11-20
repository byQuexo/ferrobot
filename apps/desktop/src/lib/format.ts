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

/** What a running tool call is doing, in a few words, for the chat. */
export function stepLabel(name: string, input: unknown, nameOf: (id: string) => string | undefined): string {
  const { server, tool } = toolLabel(name);
  const args = typeof input === "object" && input !== null ? (input as Record<string, unknown>) : {};
  const str = (key: string): string => (typeof args[key] === "string" ? args[key] : "");
  const short = (s: string): string => (s.length > 48 ? `${s.slice(0, 47)}…` : s);
  const file = (s: string): string => s.split("/").filter(Boolean).pop() ?? "a file";
  if (server === "ferrobot") {
    const connector = tool.split("__");
    if (connector.length > 1) return `Using ${connector[0] ?? tool}`;
    const to = str("to").replace(/^@/, "");
    const labels: Record<string, string> = {
      send_message: `Messaging @${nameOf(to) ?? (to || "a teammate")}`,
      create_agent: `Creating ${str("name") ? `@${str("name")}` : "an agent"}`,
      ask_user: "Writing you a question",
      list_agents: "Checking the team",
      read_mailbox: "Reading messages",
      remember: "Taking notes",
      recall: "Looking through notes",
      forget: "Tidying up notes",
    };
    return labels[tool] ?? `Using ${tool}`;
  }
  if (server) return `Using ${server}`;
  // The model's own words for what the step does (Claude's Bash, our bash tool).
  const said = str("description").trim();
  if (said) return short(said.charAt(0).toUpperCase() + said.slice(1));
  switch (tool) {
    case "Bash":
    case "bash":
      return "Working on its computer";
    case "Read":
    case "read_file":
      return `Reading ${file(str("file_path") || str("path"))}`;
    case "Write":
    case "Edit":
    case "MultiEdit":
    case "write_file":
    case "apply_patch":
      return `Editing ${file(str("file_path") || str("path"))}`;
    case "Grep":
    case "Glob":
      return "Searching files";
    case "WebSearch":
      return str("query") ? `Searching the web for ${short(str("query"))}` : "Searching the web";
    case "WebFetch":
      return `Reading ${URL.canParse(str("url")) ? new URL(str("url")).host : "a web page"}`;
    case "TodoWrite":
      return "Planning the next steps";
    default:
      return `Using ${tool}`;
  }
}
