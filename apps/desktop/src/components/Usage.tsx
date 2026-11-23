import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { ArrowDown, ArrowUp, ChartColumn, Download, Pencil, X } from "lucide-preact";

import type {
  PriceEntry,
  ProviderKind,
  ProviderLimits,
  UsageGroup,
  UsageGroupBy,
  UsageReport,
  UsageTotals,
} from "../api.ts";
import { useApp } from "../context.ts";
import {
  chartMetric,
  costLabel,
  count,
  dayLabel,
  freshTokens,
  limitCards,
  metricOf,
  niceTicks,
  rangeOf,
  shareLabel,
  slotOf,
  sortGroups,
  stackDays,
  tickLabel,
  usageCsv,
  usd,
  zoneDays,
  type DayStack,
  type Metric,
  type RangeKind,
  type SortKey,
} from "../lib/usage.ts";
import { Button, Empty, IconButton, Skeleton } from "./ui.tsx";

const RANGES: [RangeKind, string][] = [
  ["today", "Today"],
  ["7d", "7 days"],
  ["30d", "30 days"],
  ["custom", "Custom"],
];
type Breakdown = Exclude<UsageGroupBy, "provider">;
const BREAKDOWNS: [Breakdown, string][] = [
  ["model", "Model"],
  ["agent", "Agent"],
  ["day", "Day"],
];
const COLUMNS: [SortKey, string][] = [
  ["calls", "Calls"],
  ["input_tokens", "Input"],
  ["output_tokens", "Output"],
  ["cache_read_tokens", "Cache read"],
  ["cache_write_tokens", "Cache write"],
  ["cost_usd", "Est. cost"],
];

type Reports = Record<UsageGroupBy, UsageReport>;

const today = (): string => {
  const d = new Date();
  return `${String(d.getFullYear())}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
};

/** Model calls, tokens and estimated cost, from what the daemon’s model proxy saw. */
export function Usage(props: { onClose: () => void }): JSX.Element {
  const { client } = useApp();
  const [range, setRange] = useState<RangeKind>("7d");
  const [custom, setCustom] = useState({ from: today(), to: today() });
  const [reports, setReports] = useState<Reports | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [prices, setPrices] = useState<PriceEntry[]>([]);
  const [by, setBy] = useState<Breakdown>("model");
  const [sort, setSort] = useState<{ key: SortKey; desc: boolean }>({
    key: "cost_usd",
    desc: true,
  });
  const [editing, setEditing] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const [kinds, setKinds] = useState<ProviderKind[]>([]);
  const [limits, setLimits] = useState<ProviderLimits[] | { error: string } | null>(null);
  const [limitsReload, setLimitsReload] = useState(0);
  const span = rangeOf(range, new Date(), custom);

  useEffect(() => {
    client.listProviders().then(
      (list) => {
        setKinds(list.map((p) => p.kind));
      },
      () => undefined,
    );
  }, []);

  // Apart from the reports: reading a plan's limits runs the host's CLI and can take seconds.
  useEffect(() => {
    setLimits(null);
    client.limits().then(setLimits, (err: unknown) => {
      setLimits({ error: err instanceof Error ? err.message : String(err) });
    });
  }, [limitsReload]);

  useEffect(() => {
    if (!span) return;
    let live = true;
    setReports(null);
    setError(null);
    const report = (g: UsageGroupBy, daily = false): Promise<UsageReport> =>
      client.usage(g, span.from, span.to, daily);
    const load = async (): Promise<void> => {
      try {
        const [day, provider, model, agent, p] = await Promise.all([
          report("day"),
          report("provider", true),
          report("model"),
          report("agent"),
          client.prices(),
        ]);
        if (!live) return;
        setReports({ day, provider, model, agent });
        setPrices(p);
      } catch (err) {
        if (live) setError(err instanceof Error ? err.message : String(err));
      }
    };
    void load();
    return () => {
      live = false;
    };
  }, [span?.from, span?.to, reload]);

  const groups = reports ? sortGroups(reports[by].groups, sort.key, sort.desc) : [];
  const byLabel = BREAKDOWNS.find(([id]) => id === by)?.[1] ?? "";
  const timeZone = reports?.provider.timezone ?? "UTC";
  const exportCsv = (): void => {
    const url = URL.createObjectURL(new Blob([usageCsv(groups, by)], { type: "text/csv" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = `ferrobot-usage-by-${by}.csv`;
    a.click();
    setTimeout(() => {
      URL.revokeObjectURL(url);
    }, 1000);
  };

  const totals = reports?.provider.totals;
  const metric = totals ? chartMetric(totals) : "cost";
  const dates = span ? zoneDays(span.from, span.to, new Date(), timeZone) : [];
  const stack = reports ? stackDays(reports.provider.groups, dates, metric) : null;

  return (
    <main class="usage-page" aria-label="Usage">
      <section class="settings-body">
        <header class="settings-head">
          <div>
            <h2>Usage</h2>
            <p class="muted">
              Model calls your agents made through the daemon. OpenRouter costs are what it charged;
              the rest are estimates from list prices.
            </p>
            {kinds.includes("claude_code") && (
              <p class="muted small">
                Claude Code costs are estimates at API prices; your Claude subscription covers them.
              </p>
            )}
          </div>
          <IconButton label="Close usage" onClick={props.onClose}>
            <X size={18} />
          </IconButton>
        </header>
        <PlanLimits
          limits={limits}
          expected={kinds.includes("claude_code") || kinds.includes("codex")}
          onRetry={() => {
            setLimitsReload((n) => n + 1);
          }}
        />
        <div class="usage-bar">
          <div class="chips" role="group" aria-label="Time range">
            {RANGES.map(([id, label]) => (
              <button
                key={id}
                type="button"
                class={`chip${range === id ? " on" : ""}`}
                aria-pressed={range === id}
                onClick={() => {
                  setRange(id);
                }}
              >
                {label}
              </button>
            ))}
          </div>
          {range === "custom" && (
            <div class="usage-dates">
              <input
                type="date"
                aria-label="From"
                value={custom.from}
                onInput={(e) => {
                  setCustom({ ...custom, from: e.currentTarget.value });
                }}
              />
              <span class="muted">to</span>
              <input
                type="date"
                aria-label="To"
                value={custom.to}
                onInput={(e) => {
                  setCustom({ ...custom, to: e.currentTarget.value });
                }}
              />
            </div>
          )}
        </div>
        {!span ? (
          <p class="hint bad">Pick a start day on or before the end day.</p>
        ) : error ? (
          <Empty icon={<ChartColumn size={28} />} title="Usage could not be loaded">
            <p>{error}</p>
            <Button
              small
              onClick={() => {
                setReload((n) => n + 1);
              }}
            >
              Retry
            </Button>
          </Empty>
        ) : !reports || !totals || !stack ? (
          <Skeleton lines={8} />
        ) : totals.calls === 0 ? (
          <Empty icon={<ChartColumn size={28} />} title="No model calls in this range" />
        ) : (
          <>
            <Summary report={reports.provider} stack={stack} metric={metric} />
            <DayChart stack={stack} metric={metric} dates={dates} />
            <TokenTotals totals={totals} />
            <div class="usage-bar">
              <div class="tabs usage-tabs" role="tablist" aria-label="Break down by">
                {BREAKDOWNS.map(([id, label]) => (
                  <button
                    key={id}
                    type="button"
                    role="tab"
                    aria-selected={by === id}
                    class={by === id ? "active" : ""}
                    onClick={() => {
                      setBy(id);
                      setEditing(null);
                    }}
                  >
                    {label}
                  </button>
                ))}
              </div>
              <span class="spacer" />
              <Button small onClick={exportCsv}>
                <Download size={16} /> Export CSV
              </Button>
            </div>
            <table class="usage-table">
              <thead>
                <tr>
                  <SortHeader label={byLabel} column="label" sort={sort} onSort={setSort} />
                  {COLUMNS.map(([key, label]) => (
                    <SortHeader key={key} label={label} column={key} sort={sort} onSort={setSort} />
                  ))}
                  {by === "model" && <th aria-label="Price" />}
                </tr>
              </thead>
              <tbody>
                {groups.map((g) => (
                  <Row
                    key={g.key}
                    group={g}
                    editable={by === "model"}
                    editing={editing === g.key}
                    onEdit={() => {
                      setEditing(editing === g.key ? null : g.key);
                    }}
                    prices={prices}
                    onSaved={() => {
                      setEditing(null);
                      setReload(reload + 1);
                    }}
                  />
                ))}
              </tbody>
            </table>
            <p class="muted small">
              Unknown: no price for that model (set one with the pencil on the Model tab). Not
              tracked: the API reported no tokens. Chart days are in {timeZone}; Day rows are UTC
              dates.
            </p>
          </>
        )}
      </section>
    </main>
  );
}

/** The range's total and each provider's part of it, in the chart's colours. */
function Summary(props: { report: UsageReport; stack: DayStack; metric: Metric }): JSX.Element {
  const t = props.report.totals;
  const providers = [...props.report.groups].sort(
    (a, b) => metricOf(b, props.metric) - metricOf(a, props.metric),
  );
  return (
    <>
      <div class="usage-hero">
        <b class="usage-total">{costLabel(t)}</b>
        <span class="muted small">
          {t.calls.toLocaleString("en")} calls · estimate · USD
          {t.unknown_cost_calls > 0 && ` · ${String(t.unknown_cost_calls)} with an unknown price`}
          {t.not_tracked_calls > 0 && ` · ${String(t.not_tracked_calls)} not tracked`}
        </span>
      </div>
      <ul class="usage-providers" aria-label="Providers">
        {providers.map((g) => (
          <li key={g.key}>
            <i class={`usage-swatch ${slotOf(props.stack.series, g.key)}`} />
            <span class="usage-name" title={g.label}>
              {g.label}
            </span>
            <b class="num">{costLabel(g)}</b>
            <span class="num muted">
              {shareLabel(metricOf(g, props.metric), metricOf(t, props.metric))}
            </span>
            <span class="num muted">{count(metricOf(g, "tokens"))} tokens</span>
          </li>
        ))}
      </ul>
    </>
  );
}

function TokenTotals(props: { totals: UsageTotals }): JSX.Element {
  const t = props.totals;
  const cells: [string, number][] = [
    ["Tokens without cache reads", freshTokens(t)],
    ["Cache reads", t.cache_read_tokens],
    ["Uncached input", t.input_tokens],
    ["Cache writes", t.cache_write_tokens],
    ["Output", t.output_tokens],
  ];
  return (
    <div class="usage-tokens">
      {cells.map(([label, n]) => (
        <div key={label}>
          <span class="muted small">{label}</span>
          <b>{count(n)}</b>
        </div>
      ))}
    </div>
  );
}

/** How much of each plan's 5-hour window and week is used, and when each starts over. */
function PlanLimits(props: {
  limits: ProviderLimits[] | { error: string } | null;
  /** A provider with a plan exists, so a reading is on its way. */
  expected: boolean;
  onRetry: () => void;
}): JSX.Element | null {
  const { limits, expected } = props;
  if (limits === null) {
    return expected ? (
      <section class="limits" aria-label="Plan limits">
        <div class="limit-card">
          <span class="muted small">Reading your plan limits…</span>
          <Skeleton lines={2} />
        </div>
      </section>
    ) : null;
  }
  if (!Array.isArray(limits) && !expected) return null;
  const cards = Array.isArray(limits)
    ? limitCards(limits)
    : [{ key: "", title: "Plan limits", error: limits.error, rows: [] }];
  if (cards.length === 0) return null;
  return (
    <section class="limits" aria-label="Plan limits">
      {cards.map((card) => (
        <div key={card.key} class="limit-card">
          <h3 class="limit-title">{card.title}</h3>
          {card.error !== undefined ? (
            <div class="limit-error">
              <span class="small">Limits could not be read: {card.error}</span>
              <Button small onClick={props.onRetry}>
                Try again
              </Button>
            </div>
          ) : card.rows.length === 0 ? (
            <span class="muted small">No limits reported.</span>
          ) : (
            card.rows.map((row) => (
              <div key={row.label} class="limit-row">
                <div class="limit-line">
                  <span>{row.label}</span>
                  <b>{row.percent}% used</b>
                </div>
                <div class={`limit-bar ${row.level}`} aria-hidden="true">
                  <i style={{ width: `${String(row.percent)}%` }} />
                </div>
                {row.resets && <span class="muted small">Resets {row.resets}</span>}
              </div>
            ))
          )}
        </div>
      ))}
    </section>
  );
}

function SortHeader(props: {
  label: string;
  column: SortKey;
  sort: { key: SortKey; desc: boolean };
  onSort: (s: { key: SortKey; desc: boolean }) => void;
}): JSX.Element {
  const active = props.sort.key === props.column;
  return (
    <th
      class={props.column === "label" ? "" : "num"}
      aria-sort={active ? (props.sort.desc ? "descending" : "ascending") : "none"}
    >
      <button
        type="button"
        onClick={() => {
          props.onSort({
            key: props.column,
            desc: active ? !props.sort.desc : props.column !== "label",
          });
        }}
      >
        {props.label}
        {active && (props.sort.desc ? <ArrowDown size={12} /> : <ArrowUp size={12} />)}
      </button>
    </th>
  );
}

const [LEFT, RIGHT, TOP, PLOT, AXIS] = [56, 16, 8, 160, 24];

/** A rectangle path with its top corners rounded by `r`. */
function columnPath(x: number, y: number, w: number, h: number, r: number): string {
  const c = Math.min(r, w / 2, h);
  return ["M", x, y + h, "V", y + c, "A", c, c, 0, 0, 1, x + c, y, "H", x + w - c]
    .concat(["A", c, c, 0, 0, 1, x + w, y + c, "V", y + h, "Z"])
    .join(" ");
}

/** Estimated cost per day (tokens when no cost is known), stacked by provider. */
function DayChart(props: { stack: DayStack; metric: Metric; dates: string[] }): JSX.Element {
  const box = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [active, setActive] = useState<number | null>(null);
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setWidth(entry.contentRect.width);
    });
    observer.observe(el);
    return () => {
      observer.disconnect();
    };
  }, []);

  const { metric } = props;
  const { series, columns } = props.stack;
  const ticks = niceTicks(Math.max(0, ...columns.map((c) => c.total)));
  const [top, step] = [ticks[ticks.length - 1] ?? 1, ticks[1] ?? 1];
  const band = Math.max(width - LEFT - RIGHT, 0) / Math.max(columns.length, 1);
  const bar = Math.min(24, band * 0.7);
  const every = Math.ceil(56 / Math.max(band, 1));
  const y = (v: number): number => TOP + PLOT - (v / top) * PLOT;
  const center = (i: number): number => LEFT + band * (i + 0.5);
  const value = (n: number): string => (metric === "cost" ? usd(n) : count(n));
  const slot = (s: number): string => slotOf(series, series[s]?.key ?? "");
  const title = `${metric === "cost" ? "Estimated cost" : "Tokens"} per day${series.length > 1 ? " by provider" : ""}`;
  const peak = columns.reduce<(typeof columns)[number] | undefined>(
    (a, c) => (!a || c.total > a.total ? c : a),
    undefined,
  );
  const [first, last] = [props.dates[0] ?? "", props.dates[props.dates.length - 1] ?? ""];
  const summary = `${title}, ${dayLabel(first)} to ${dayLabel(last)}${peak ? `; highest ${dayLabel(peak.date)} at ${value(peak.total)}` : ""}.`;
  const at = active === null ? undefined : columns[active];
  const flip = active !== null && center(active) > width / 2;
  const keys: Record<string, number> = {
    ArrowLeft: (active ?? columns.length) - 1,
    ArrowRight: (active ?? -1) + 1,
    Home: 0,
    End: columns.length - 1,
  };

  const segments = (c: (typeof columns)[number], x: number): JSX.Element[] => {
    const height = c.total > 0 ? Math.max(2, (c.total / top) * PLOT) : 0;
    const topmost = c.values.reduce((t, v, s) => (v > 0 ? s : t), -1);
    let bottom = TOP + PLOT;
    return c.values.flatMap((v, s) => {
      if (v <= 0) return [];
      const h = (height * v) / c.total;
      const gap = bottom < TOP + PLOT ? 2 : 0;
      bottom -= h;
      if (h - gap < 0.5) return [];
      const d = columnPath(x, bottom, bar, h - gap, s === topmost ? 4 : 0);
      return [<path key={s} class={slot(s)} d={d} />];
    });
  };

  return (
    <figure class="usage-chart">
      <figcaption class="muted small">{title}</figcaption>
      <div class="usage-plot" ref={box}>
        {width > 0 && (
          <svg
            width={width}
            height={TOP + PLOT + AXIS}
            role="img"
            aria-label={summary}
            tabIndex={0}
            onPointerMove={(e) => {
              const x = e.clientX - e.currentTarget.getBoundingClientRect().left - LEFT;
              const i = Math.floor(x / band);
              setActive(i >= 0 && i < columns.length ? i : null);
            }}
            onPointerLeave={() => {
              setActive(null);
            }}
            onFocus={() => {
              setActive((a) => a ?? columns.length - 1);
            }}
            onBlur={() => {
              setActive(null);
            }}
            onKeyDown={(e) => {
              const to = keys[e.key];
              if (to === undefined) return;
              e.preventDefault();
              setActive(Math.min(columns.length - 1, Math.max(0, to)));
            }}
          >
            {active !== null && (
              <rect class="hover" x={LEFT + band * active} y={TOP} width={band} height={PLOT} />
            )}
            {ticks.map((t) => (
              <g key={t}>
                <line class={t === 0 ? "base" : "grid"} x1={LEFT} x2={width} y1={y(t)} y2={y(t)} />
                <text class="tick" x={LEFT - 8} y={y(t)}>
                  {tickLabel(t, step, metric)}
                </text>
              </g>
            ))}
            {columns.map((c, i) => (
              <g key={c.date}>
                {segments(c, center(i) - bar / 2)}
                {(columns.length - 1 - i) % every === 0 && (
                  <text x={center(i)} y={TOP + PLOT + 18}>
                    {dayLabel(c.date)}
                  </text>
                )}
              </g>
            ))}
          </svg>
        )}
        <div aria-live="polite">
          {at && active !== null && (
            <div
              class={`usage-tip${flip ? " flip" : ""}`}
              style={{ left: center(active) + (flip ? -1 : 1) * (bar / 2 + 8) }}
            >
              <span class="muted">{dayLabel(at.date)}</span>
              <b>
                {value(at.total)}
                {metric === "cost" && at.unknown ? " + unknown" : ""}
              </b>
              {series.length > 1 &&
                series.map((s, i) => (
                  <span key={s.key}>
                    <i class={`usage-key ${slot(i)}`} />
                    <b>{value(at.values[i] ?? 0)}</b> {s.label}
                  </span>
                ))}
            </div>
          )}
        </div>
      </div>
    </figure>
  );
}

function Row(props: {
  group: UsageGroup;
  editable: boolean;
  editing: boolean;
  onEdit: () => void;
  prices: PriceEntry[];
  onSaved: () => void;
}): JSX.Element {
  const g = props.group;
  return (
    <>
      <tr>
        <td class="usage-name" title={g.label}>
          {g.label}
          {props.editable && (
            <span class="muted small"> · {g.key.slice(0, g.key.indexOf("/"))}</span>
          )}
        </td>
        <td class="num">
          {g.calls.toLocaleString("en")}
          {g.not_tracked_calls > 0 && (
            <span class="muted small"> · {g.not_tracked_calls} not tracked</span>
          )}
        </td>
        <td class="num">{count(g.input_tokens)}</td>
        <td class="num">{count(g.output_tokens)}</td>
        <td class="num">{count(g.cache_read_tokens)}</td>
        <td class="num">{count(g.cache_write_tokens)}</td>
        <td class={`num${g.cost_usd == null ? " muted" : ""}`}>{costLabel(g)}</td>
        {props.editable && (
          <td>
            <IconButton
              label={`Edit the price of ${g.label}`}
              pressed={props.editing}
              onClick={props.onEdit}
            >
              <Pencil size={16} />
            </IconButton>
          </td>
        )}
      </tr>
      {props.editing && (
        <tr>
          <td colSpan={8}>
            <PriceForm modelKey={g.key} prices={props.prices} onSaved={props.onSaved} />
          </td>
        </tr>
      )}
    </>
  );
}

/** The operator’s own price for one model; saved with the other overrides. */
function PriceForm(props: {
  modelKey: string;
  prices: PriceEntry[];
  onSaved: () => void;
}): JSX.Element {
  const { client, run } = useApp();
  const slash = props.modelKey.indexOf("/");
  const kind = props.modelKey.slice(0, slash) as ProviderKind;
  const model = props.modelKey.slice(slash + 1);
  const current = props.prices.find((p) => p.provider_kind === kind && p.model === model);
  const text = (n: number | null | undefined): string => (n == null ? "" : String(n));
  const [values, setValues] = useState({
    input: text(current?.input),
    output: text(current?.output),
    cache_read: text(current?.cache_read),
    cache_write: text(current?.cache_write),
  });
  const number = (s: string): number | null => (s.trim() === "" ? null : Number(s));
  const valid =
    [values.input, values.output].every((v) => v.trim() !== "" && Number(v) >= 0) &&
    [values.cache_read, values.cache_write].every((v) => v.trim() === "" || Number(v) >= 0);

  const save = async (): Promise<void> => {
    const others = props.prices
      .filter((p) => p.source == null && !(p.provider_kind === kind && p.model === model))
      .map(({ provider_kind, model: m, input, output, cache_read, cache_write }) => ({
        provider_kind,
        model: m,
        input,
        output,
        cache_read,
        cache_write,
      }));
    const saved = await run(() =>
      client.setPrices([
        ...others,
        {
          provider_kind: kind,
          model,
          input: Number(values.input),
          output: Number(values.output),
          cache_read: number(values.cache_read),
          cache_write: number(values.cache_write),
        },
      ]),
    );
    if (saved) props.onSaved();
  };

  return (
    <form
      class="usage-price"
      onSubmit={(e) => {
        e.preventDefault();
        if (valid) void save();
      }}
    >
      <span class="muted small">
        USD per 1M tokens for {model}
        {current?.source
          ? ` · now from ${new URL(current.source).host}, checked ${current.checked ?? "?"}`
          : current
            ? " · your price"
            : " · no price yet"}
      </span>
      {(["input", "output", "cache_read", "cache_write"] as const).map((field) => (
        <label key={field}>
          {field.replace("_", " ")}
          <input
            type="number"
            min="0"
            step="any"
            required={field === "input" || field === "output"}
            value={values[field]}
            onInput={(e) => {
              const value = e.currentTarget.value;
              setValues((v) => ({ ...v, [field]: value }));
            }}
          />
        </label>
      ))}
      <Button small type="submit" disabled={!valid}>
        Save price
      </Button>
    </form>
  );
}
