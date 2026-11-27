import type { AgentContext } from "../api.ts";
import { count } from "./usage.ts";

const MAX_TAGS = 5;
const MAX_TAG_CHARS = 20;

/** `tags` with `raw` added under the daemon's rules, or why it can't be. */
export function addTag(
  tags: readonly string[],
  raw: string,
): { tags: string[] } | { error: string } {
  const tag = raw.trim();
  // Code points, as the daemon counts them.
  const chars = Array.from(tag).length;
  if (chars === 0 || chars > MAX_TAG_CHARS) {
    return { error: `A tag has 1 to ${String(MAX_TAG_CHARS)} characters` };
  }
  if (tags.includes(tag)) return { error: `“${tag}” is already there` };
  if (tags.length >= MAX_TAGS) return { error: `At most ${String(MAX_TAGS)} tags` };
  return { tags: [...tags, tag] };
}

/** What the context meter shows; `percent` is 0-100, `null` when it can't be known. */
export function contextMeter(ctx: AgentContext | null): {
  percent: number | null;
  title: string;
  detail: string;
} {
  if (!ctx) return { percent: null, title: "Context", detail: "Loading…" };
  if (!ctx.tracked) {
    return {
      percent: null,
      title: "Context not tracked",
      detail: "Claude Code runs on your own login, so ferrobot doesn’t see its model calls.",
    };
  }
  const tokens = ctx.tokens ?? 0;
  if (!ctx.window) {
    return {
      percent: null,
      title: "Context size unknown",
      detail: `${count(tokens)} tokens; the window of ${ctx.model ?? "this model"} is unknown.`,
    };
  }
  const percent = Math.min(100, Math.round((tokens / ctx.window) * 100));
  return {
    percent,
    title: `Context ${String(percent)}% full`,
    detail: `${count(tokens)} of ${count(ctx.window)} tokens`,
  };
}

type ContextPart = AgentContext["parts"][number];

/** One row of the context breakdown; `percent` is of the window, `null` for deferred tools. */
export interface ContextRow {
  label: string;
  tokens: number;
  percent: number | null;
  /** Colour class of its bar segment and dot. */
  slot: string;
  deferred: boolean;
}

/** What the breakdown shows: rows (used parts, then free space), the MCP tools and a note. */
export interface ContextBreakdown {
  rows: ContextRow[];
  tools: { name: string; tokens: number }[];
  note: string;
  /** Whether "Measure now" can fill it in (Claude Code agents without a breakdown yet). */
  measurable: boolean;
}

const SLOTS: Record<ContextPart["kind"], string> = {
  system_prompt: "c-system",
  system_tools: "c-tools",
  mcp_tools: "c-mcp",
  custom_agents: "c-extra",
  memory_files: "c-extra",
  skills: "c-extra",
  messages: "c-messages",
  autocompact_buffer: "c-buffer",
  free_space: "c-free",
  other: "c-extra",
};

function share(tokens: number, window: number | null | undefined): number | null {
  return window ? Math.round((tokens / window) * 1000) / 10 : null;
}

/** The context's parts as rows, or why there are none; `null` before anything is known. */
export function contextBreakdown(ctx: AgentContext | null): ContextBreakdown | null {
  if (!ctx?.tracked || ctx.tokens == null) return null;
  if (!ctx.source || ctx.parts.length === 0) {
    return {
      rows: [],
      tools: [],
      note: ctx.compact
        ? "The breakdown comes from Claude Code a moment after each turn, or measure it now."
        : "The breakdown comes with the agent’s next model call through ferrobot.",
      measurable: ctx.compact,
    };
  }
  const window = ctx.window;
  const rows: ContextRow[] = ctx.parts.map((p) => ({
    label: p.label,
    tokens: p.tokens,
    percent: p.deferred ? null : share(p.tokens, window),
    slot: SLOTS[p.kind],
    deferred: p.deferred,
  }));
  // Claude Code names its free space; for the others it is what the exact total leaves.
  if (window && !ctx.parts.some((p) => p.kind === "free_space")) {
    const free = Math.max(0, window - ctx.tokens);
    rows.push({
      label: "Free space",
      tokens: free,
      percent: share(free, window),
      slot: SLOTS.free_space,
      deferred: false,
    });
  }
  const leftOut = ctx.left_out
    .map((l) => (l === "mcp_tools" ? "MCP tools" : "ferrobot’s instructions"))
    .join(" and ");
  let note =
    ctx.source === "claude_code"
      ? "Parts as Claude Code’s /context counts them, rounded."
      : "Parts estimated from the size of the newest request; the total is exact.";
  if (leftOut) note += ` Measured outside a turn, without ${leftOut}; the next turn counts them.`;
  return { rows, tools: ctx.tools, note, measurable: false };
}
