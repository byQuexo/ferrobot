import type { UsageGroup, UsageTotals } from "../api.ts";

export type RangeKind = "today" | "7d" | "30d" | "custom";

/** `[from, to)` as ISO times for a range of local days; `to` null means now. Custom days are inclusive. */
export function rangeOf(
  kind: RangeKind,
  now: Date,
  custom: { from: string; to: string },
): { from: string; to: string | null } | null {
  const day = (d: Date, plus = 0): Date =>
    new Date(d.getFullYear(), d.getMonth(), d.getDate() + plus);
  switch (kind) {
    case "today":
      return { from: day(now).toISOString(), to: null };
    case "7d":
      return { from: day(now, -6).toISOString(), to: null };
    case "30d":
      return { from: day(now, -29).toISOString(), to: null };
    case "custom": {
      const [start, end] = [localDay(custom.from), localDay(custom.to)];
      if (!start || !end || end < start) return null;
      return { from: start.toISOString(), to: day(end, 1).toISOString() };
    }
  }
}

function localDay(s: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s);
  return m ? new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])) : null;
}

/** Every UTC date the range touches, so days without calls still get a bar. */
export function utcDays(from: string, to: string | null, now: Date): string[] {
  const end = to ? new Date(Date.parse(to) - 1) : now;
  const days: string[] = [];
  for (
    let t = Date.parse(from.slice(0, 10));
    t <= end.getTime() && days.length < 400;
    t += 86_400_000
  ) {
    days.push(new Date(t).toISOString().slice(0, 10));
  }
  return days;
}

export type SortKey =
  | "label"
  | "calls"
  | "input_tokens"
  | "output_tokens"
  | "cache_read_tokens"
  | "cache_write_tokens"
  | "cost_usd";

/** Unknown costs sort last in both directions. */
export function sortGroups(groups: UsageGroup[], key: SortKey, desc: boolean): UsageGroup[] {
  const value = (g: UsageGroup): string | number | null | undefined =>
    key === "label" ? g.label.toLowerCase() : g[key];
  return [...groups].sort((a, b) => {
    const [x, y] = [value(a), value(b)];
    if (x == null || y == null) return Number(x == null) - Number(y == null);
    const order = x < y ? -1 : x > y ? 1 : 0;
    return desc ? -order : order;
  });
}

/** RFC 4180 CSV. Text cells that a spreadsheet would run as a formula get a leading quote. */
export function toCsv(rows: (string | number | null | undefined)[][]): string {
  const cell = (v: string | number | null | undefined): string => {
    if (v === null || v === undefined) return "";
    if (typeof v === "number") return String(v);
    const text = /^[=+\-@\t\r]/.test(v) ? `'${v}` : v;
    return /[",\r\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
  };
  return rows.map((r) => r.map(cell).join(",")).join("\r\n") + "\r\n";
}

export function usageCsv(groups: UsageGroup[], name: string): string {
  return toCsv([
    [
      name,
      "calls",
      "input tokens",
      "output tokens",
      "cache read tokens",
      "cache write tokens",
      "estimated cost (USD)",
      "calls with unknown cost",
      "calls not tracked",
    ],
    ...groups.map((g) => [
      g.label,
      g.calls,
      g.input_tokens,
      g.output_tokens,
      g.cache_read_tokens,
      g.cache_write_tokens,
      g.cost_usd ?? null,
      g.unknown_cost_calls,
      g.not_tracked_calls,
    ]),
  ]);
}

export function usd(n: number): string {
  return n > 0 && n < 0.01 ? "<$0.01" : `$${n.toFixed(2)}`;
}

/** "$1.20", "$1.20 + unknown", "unknown" or "not tracked". */
export function costLabel(t: UsageTotals): string {
  if (t.cost_usd == null) return t.unknown_cost_calls > 0 ? "unknown" : "not tracked";
  return t.unknown_cost_calls > 0 ? `${usd(t.cost_usd)} + unknown` : usd(t.cost_usd);
}

const compact = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 });

export function count(n: number): string {
  return compact.format(n);
}
