import type { Agent, Presence } from "../api.ts";
import { mentionsAsText } from "./mentions.ts";

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

export function credentialLabel(kind: "missing" | "rejected"): string {
  return kind === "missing" ? "Key missing" : "Key rejected";
}

export const errorText = (err: unknown): string =>
  err instanceof Error ? err.message : String(err);

export const plural = (n: number, word: string): string =>
  `${String(n)} ${word}${n === 1 ? "" : "s"}`;

export const TOOLS_UNSUPPORTED = "Model can't use tools";

/** The agent's status dot and label: a model credential or tools problem outranks presence. */
export function statusOf(a: Pick<Agent, "presence" | "credential_error" | "tools_unsupported">): {
  presence: Presence;
  label: string;
} {
  if (a.credential_error)
    return { presence: "error", label: credentialLabel(a.credential_error.kind) };
  if (a.tools_unsupported) return { presence: "error", label: TOOLS_UNSUPPORTED };
  return { presence: a.presence, label: presenceLabel(a.presence) };
}

/** The agent dialog's tools summary: "All tools", "7 of 9 tools" or "No tools", plus connectors given. */
export function toolsSummary(on: number, total: number, connectors: number): string {
  const tools =
    on >= total ? "All tools" : on === 0 ? "No tools" : `${String(on)} of ${String(total)} tools`;
  if (!connectors) return tools;
  return `${tools} + ${plural(connectors, "connector")}`;
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
  return Number.isNaN(d.getTime())
    ? ""
    : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
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

const COMPUTER_TOOLS = new Set(["bash", "read_file", "write_file"]);

/** The agent list's second line: a problem, an open card, work, then the newest reply. */
export function rowLine(
  a: Pick<
    Agent,
    "presence" | "credential_error" | "tools_unsupported" | "waiting" | "last_reply" | "description"
  >,
  now: number = Date.now(),
  nameOf: (id: string) => string | undefined = () => undefined,
): { text: string; tone: "bad" | "wait" | ""; time: string } {
  if (a.credential_error || a.tools_unsupported || a.presence === "error")
    return { text: statusOf(a).label, tone: "bad", time: "" };
  if (a.waiting > 0) {
    const text =
      a.waiting === 1 ? "Waiting for your answer" : `${String(a.waiting)} questions for you`;
    return { text, tone: "wait", time: "" };
  }
  if (a.presence === "working") return { text: "Working…", tone: "", time: "" };
  if (a.last_reply?.text) {
    const time = relativeTime(a.last_reply.at, now).replace(/ ago$/, "");
    // A preview, not Markdown: drop heading, emphasis, quote and code marks.
    const text = mentionsAsText(a.last_reply.text, nameOf)
      .replace(/[#*`>]+/g, "")
      .replace(/\s+/g, " ")
      .trim();
    return { text, tone: "", time };
  }
  return { text: a.description || "No description", tone: "", time: "" };
}

/** A finished run of tool calls in the chat, e.g. "3 steps · 40 s". */
export function stepsLabel(count: number, seconds: number): string {
  const steps = plural(count, "step");
  return seconds >= 1 ? `${steps} · ${duration(Math.round(seconds))}` : steps;
}

/** The Computer tab's first line, in plain words. */
export function computerSummary(
  status: Agent["status"],
  info: { idle_s: number; warm_after_s?: number | null; idle_timeout_s?: number | null },
): string {
  const after = (limit: number | null | undefined): number | null =>
    limit ? Math.max(0, limit - info.idle_s) : null;
  if (status === "paused") return "Paused · resumes quickly";
  if (status === "archived") return "Archived · restores on wake";
  if (status === "stopped") return "Asleep · a message wakes it";
  const parts = [info.idle_s < 5 ? "Busy just now" : `Idle ${duration(info.idle_s)}`];
  const pause = after(info.warm_after_s);
  const sleep = after(info.idle_timeout_s);
  if (pause !== null) parts.push(`pauses in ${duration(pause)}`);
  else if (sleep !== null) parts.push(`sleeps in ${duration(sleep)}`);
  return parts.join(" · ");
}

/** What a running tool call is doing, in a few words, for the chat. */
function desktopLabel(action: string): string {
  const labels: Record<string, string> = {
    screenshot: "Looking at its screen",
    type: "Typing on its screen",
    key: "Pressing keys on its screen",
    scroll: "Scrolling on its screen",
    move: "Moving the pointer on its screen",
  };
  return labels[action] ?? "Clicking on its screen";
}

export function stepLabel(
  name: string,
  input: unknown,
  nameOf: (id: string) => string | undefined,
): string {
  const { server, tool } = toolLabel(name);
  const args =
    typeof input === "object" && input !== null ? (input as Record<string, unknown>) : {};
  const str = (key: string): string => (typeof args[key] === "string" ? args[key] : "");
  const short = (s: string): string => (s.length > 48 ? `${s.slice(0, 47)}…` : s);
  const file = (s: string): string => s.split("/").filter(Boolean).pop() ?? "a file";
  // The Claude Code driver's computer tools come through ferrobot's MCP server.
  if (server === "ferrobot" && !COMPUTER_TOOLS.has(tool)) {
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
      desktop: desktopLabel(str("action")),
    };
    return labels[tool] ?? `Using ${tool}`;
  }
  if (server && server !== "ferrobot") return `Using ${server}`;
  // The model's own words for what the step does (Claude's Bash, our bash tool).
  const said = str("description").trim();
  if (said) return short(said.charAt(0).toUpperCase() + said.slice(1));
  switch (tool) {
    case "Bash":
    case "bash": {
      const command = str("command").trim().split("\n")[0] ?? "";
      return command ? `Running ${short(command)}` : "Working on its computer";
    }
    case "desktop":
      return desktopLabel(str("action"));
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

/** A URL without scheme or `www.`; a long one keeps host, first path segment and two query keys. */
export function linkLabel(url: string): string {
  // Split by hand: `URL` would show punycode hosts and percent-encoded paths.
  const full = url.replace(/^https?:\/\/(www\.)?/i, "").replace(/\/$/, "");
  if (full.length <= 40) return full;
  const [rest = "", hash] = full.split(/#(.*)/s);
  const [address = "", search = ""] = rest.split(/\?(.*)/s);
  const [host = "", first, ...more] = address.split("/").filter(Boolean);
  const params = search.split("&").filter(Boolean);
  const path = first === undefined ? "" : `/${first}${more.length ? "/…" : ""}`;
  const keys = params.slice(0, 2).map((p) => p.replace(/=.*/s, "=…"));
  const query = keys.length ? `?${keys.join("&")}${params.length > 2 ? "&…" : ""}` : "";
  return `${host}${path}${query}${hash === undefined ? "" : "#…"}`;
}

/** What a Stop ended beyond the agent's own turn; empty when nothing else. */
export function stopSummary(agents: number, dropped: number): string {
  const parts = [
    agents > 1 ? `${String(agents)} agents stopped` : "",
    dropped > 0 ? `${plural(dropped, "queued message")} dropped` : "",
  ].filter(Boolean);
  return parts.join(", ");
}
