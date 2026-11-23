import type { LimitWindow, ProviderKind, ProviderLimits, UsageGroup, UsageTotals } from "../api.ts";

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

/** Every date in `timeZone` the range touches, so days without calls still get a column. */
export function zoneDays(from: string, to: string | null, now: Date, timeZone: string): string[] {
  const format = new Intl.DateTimeFormat("en", {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  });
  const date = (t: number): string => {
    const p = new Map(format.formatToParts(t).map((x) => [x.type, x.value]));
    return `${p.get("year") ?? ""}-${p.get("month") ?? ""}-${p.get("day") ?? ""}`;
  };
  const last = date(to ? Date.parse(to) - 1 : now.getTime());
  const days: string[] = [];
  for (let t = Date.parse(date(Date.parse(from))); days.length < 400; t += 86_400_000) {
    const day = new Date(t).toISOString().slice(0, 10);
    if (day > last) break;
    days.push(day);
  }
  return days;
}

export type Metric = "cost" | "tokens";

/** Cost when some is known and above zero, else tokens (no prices, or only free models). */
export function chartMetric(t: UsageTotals): Metric {
  return t.cost_usd ? "cost" : "tokens";
}

export function metricOf(t: UsageTotals, metric: Metric): number {
  return metric === "cost"
    ? (t.cost_usd ?? 0)
    : t.input_tokens + t.output_tokens + t.cache_read_tokens + t.cache_write_tokens;
}

export interface DayStack {
  /** In colour order; "Other" (key "") comes last. */
  series: { key: string; label: string }[];
  /** One per date; `values` line up with `series`. */
  columns: { date: string; values: number[]; total: number; unknown: boolean }[];
}

const SERIES = 5;

/** Per-day sums of the biggest groups, the rest folded into "Other". */
export function stackDays(groups: UsageGroup[], dates: string[], metric: Metric): DayStack {
  const ranked = [...groups].sort((a, b) => metricOf(b, metric) - metricOf(a, metric));
  const shown = ranked.length > SERIES ? ranked.slice(0, SERIES - 1) : ranked;
  const rest = ranked.slice(shown.length);
  // By name, so a group keeps its colour when a new range changes its rank.
  shown.sort((a, b) => a.label.localeCompare(b.label));
  const parts = [...shown.map((g) => [g]), ...(rest.length > 0 ? [rest] : [])].map((part) =>
    part.map((g) => new Map((g.days ?? []).map((d) => [d.date, d]))),
  );
  return {
    series: [
      ...shown.map(({ key, label }) => ({ key, label })),
      ...(rest.length > 0 ? [{ key: "", label: "Other" }] : []),
    ],
    columns: dates.map((date) => {
      const days = parts.map((part) => part.flatMap((byDate) => byDate.get(date) ?? []));
      const values = days.map((part) => part.reduce((sum, d) => sum + metricOf(d, metric), 0));
      return {
        date,
        values,
        total: values.reduce((a, b) => a + b, 0),
        unknown: days.some((part) => part.some((d) => d.unknown_cost_calls > 0)),
      };
    }),
  };
}

/** Axis ticks from 0 to at least `max` in steps of 1, 2 or 5 times a power of ten. */
export function niceTicks(max: number, target = 4): number[] {
  if (!(max > 0)) return [0, 1];
  const raw = max / target;
  const power = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * power).find((s) => s >= raw) ?? raw;
  return Array.from({ length: Math.ceil(max / step - 1e-9) + 1 }, (_, i) =>
    Number((i * step).toPrecision(12)),
  );
}

export function tickLabel(n: number, step: number, metric: Metric): string {
  if (metric === "tokens") return count(n);
  const digits = step >= 1 ? 0 : Math.max(2, Math.ceil(-Math.log10(step) - 1e-9));
  return `$${n.toLocaleString("en", { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
}

const shortDate = new Intl.DateTimeFormat("en", {
  month: "short",
  day: "numeric",
  timeZone: "UTC",
});

/** "Nov 19" for "2026-11-19". */
export function dayLabel(date: string): string {
  return shortDate.format(Date.parse(date));
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

/** Colour class of a group in a stack: `s0`…`s4`, or `other` once folded. */
export function slotOf(series: DayStack["series"], key: string): string {
  const i = series.findIndex((s) => s.key === key);
  return i < 0 || key === "" ? "other" : `s${String(i)}`;
}

/** Everything but cache reads, which cost a fraction of fresh input. */
export function freshTokens(t: UsageTotals): number {
  return t.input_tokens + t.cache_write_tokens + t.output_tokens;
}

/** "42%", "<1%" or "0%". */
export function shareLabel(part: number, total: number): string {
  if (!(total > 0) || part <= 0) return "0%";
  const pct = (part / total) * 100;
  return pct < 1 ? "<1%" : `${String(Math.round(pct))}%`;
}

const resetTime = new Intl.DateTimeFormat("en", {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

/** A Codex reset (RFC 3339) in local time; Claude Code's own words keep their zone unless it is `zone`. */
export function resetLabel(
  resets: string,
  zone = Intl.DateTimeFormat().resolvedOptions().timeZone,
): string {
  if (/^\d{4}-\d{2}-\d{2}T/.test(resets)) return resetTime.format(Date.parse(resets));
  return resets.replace(` (${zone})`, "");
}

export interface LimitRow {
  label: string;
  /** 0-100, rounded. */
  percent: number;
  /** `high` from 75%, `full` from 90%. */
  level: "ok" | "high" | "full";
  resets?: string;
}

export interface LimitCard {
  key: string;
  title: string;
  error?: string;
  rows: LimitRow[];
}

const PLAN_TITLES: Partial<Record<ProviderKind, string>> = {
  claude_code: "Claude Code",
  codex: "Codex",
};

/** One card per plan: Claude Code providers share the host's one login, so they show once. */
export function limitCards(limits: ProviderLimits[], zone?: string): LimitCard[] {
  const seen = new Set<string>();
  return limits.flatMap((l) => {
    const key = l.kind === "claude_code" ? l.kind : l.provider_id;
    if (seen.has(key)) return [];
    seen.add(key);
    const windows: [string, LimitWindow | null | undefined][] = [
      ["5-hour window", l.five_hour],
      ["Weekly", l.weekly],
    ];
    const rows = windows.flatMap(([label, w]): LimitRow[] => {
      if (!w) return [];
      const percent = Math.round(Math.min(100, Math.max(0, w.used_percent)));
      const level = percent >= 90 ? "full" : percent >= 75 ? "high" : "ok";
      return [
        { label, percent, level, ...(w.resets ? { resets: resetLabel(w.resets, zone) } : {}) },
      ];
    });
    return [
      { key, title: PLAN_TITLES[l.kind] ?? l.name, rows, ...(l.error ? { error: l.error } : {}) },
    ];
  });
}
