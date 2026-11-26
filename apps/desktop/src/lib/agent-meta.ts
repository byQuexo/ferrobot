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
