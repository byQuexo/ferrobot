import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { ArrowDown, ArrowUp, ChartColumn, Download, Pencil, X } from "lucide-preact";

import type { PriceEntry, ProviderKind, UsageGroup, UsageGroupBy, UsageReport } from "../api.ts";
import { useApp } from "../context.ts";
import { costLabel, count, rangeOf, sortGroups, usageCsv, usd, utcDays, type RangeKind, type SortKey } from "../lib/usage.ts";
import { Button, Empty, IconButton, Skeleton } from "./ui.tsx";

const RANGES: [RangeKind, string][] = [
  ["today", "Today"],
  ["7d", "7 days"],
  ["30d", "30 days"],
  ["custom", "Custom"],
];
type Breakdown = Exclude<UsageGroupBy, "day">;
const BREAKDOWNS: [Breakdown, string][] = [
  ["provider", "Provider"],
  ["model", "Model"],
  ["agent", "Agent"],
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
  const { client, toast } = useApp();
  const [range, setRange] = useState<RangeKind>("7d");
  const [custom, setCustom] = useState({ from: today(), to: today() });
  const [reports, setReports] = useState<Reports | null>(null);
  const [prices, setPrices] = useState<PriceEntry[]>([]);
  const [by, setBy] = useState<Breakdown>("provider");
  const [sort, setSort] = useState<{ key: SortKey; desc: boolean }>({ key: "cost_usd", desc: true });
  const [editing, setEditing] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const span = rangeOf(range, new Date(), custom);

  useEffect(() => {
    if (!span) return;
    let live = true;
    setReports(null);
    const report = (g: UsageGroupBy): Promise<UsageReport> => client.usage(g, span.from, span.to);
    Promise.all([report("day"), report("provider"), report("model"), report("agent"), client.prices()]).then(
      ([day, provider, model, agent, p]) => {
        if (!live) return;
        setReports({ day, provider, model, agent });
        setPrices(p);
      },
      (err: unknown) => { toast(err instanceof Error ? err.message : String(err)); },
    );
    return () => { live = false; };
  }, [span?.from, span?.to, reload]);

  const groups = reports ? sortGroups(reports[by].groups, sort.key, sort.desc) : [];
  const exportCsv = (): void => {
    const url = URL.createObjectURL(new Blob([usageCsv(groups, by)], { type: "text/csv" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = `ferrobot-usage-by-${by}.csv`;
    a.click();
    setTimeout(() => { URL.revokeObjectURL(url); }, 1000);
  };

  return (
    <main class="usage-page" aria-label="Usage">
      <section class="settings-body">
        <header class="settings-head">
          <div>
            <h2>Usage</h2>
            <p class="muted">Model calls your agents made through the daemon. Costs are estimates from list prices.</p>
          </div>
          <IconButton label="Close usage" onClick={props.onClose}>
            <X size={18} />
          </IconButton>
        </header>
        <div class="usage-bar">
          <div class="chips" role="group" aria-label="Time range">
            {RANGES.map(([id, label]) => (
              <button key={id} type="button" class={`chip${range === id ? " on" : ""}`} aria-pressed={range === id} onClick={() => { setRange(id); }}>
                {label}
              </button>
            ))}
          </div>
          {range === "custom" && (
            <div class="usage-dates">
              <input type="date" aria-label="From" value={custom.from} onInput={(e) => { setCustom({ ...custom, from: e.currentTarget.value }); }} />
              <span class="muted">to</span>
              <input type="date" aria-label="To" value={custom.to} onInput={(e) => { setCustom({ ...custom, to: e.currentTarget.value }); }} />
            </div>
          )}
        </div>
        {!span ? (
          <p class="hint bad">Pick a start day on or before the end day.</p>
        ) : !reports ? (
          <Skeleton lines={8} />
        ) : reports.day.totals.calls === 0 ? (
          <Empty icon={<ChartColumn size={36} />} title="No model calls in this range" />
        ) : (
          <>
            <Totals report={reports.day} />
            <DayChart report={reports.day} days={utcDays(span.from, span.to, new Date())} />
            <div class="usage-bar">
              <div class="chips" role="group" aria-label="Break down by">
                {BREAKDOWNS.map(([id, label]) => (
                  <button key={id} type="button" class={`chip${by === id ? " on" : ""}`} aria-pressed={by === id} onClick={() => { setBy(id); setEditing(null); }}>
                    By {label.toLowerCase()}
                  </button>
                ))}
              </div>
              <span class="spacer" />
              <Button small onClick={exportCsv}>
                <Download size={14} /> Export CSV
              </Button>
            </div>
            <table class="usage-table">
              <thead>
                <tr>
                  <SortHeader label={BREAKDOWNS.find(([id]) => id === by)?.[1] ?? ""} column="label" sort={sort} onSort={setSort} />
                  {COLUMNS.map(([key, label]) => <SortHeader key={key} label={label} column={key} sort={sort} onSort={setSort} />)}
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
                    onEdit={() => { setEditing(editing === g.key ? null : g.key); }}
                    prices={prices}
                    onSaved={() => { setEditing(null); setReload(reload + 1); }}
                  />
                ))}
              </tbody>
            </table>
            <p class="muted small">
              Unknown: no price for that model (set one with the pencil on the model list). Not tracked: the API
              reported no tokens. Days are UTC.
            </p>
          </>
        )}
      </section>
    </main>
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
    <th class={props.column === "label" ? "" : "num"} aria-sort={active ? (props.sort.desc ? "descending" : "ascending") : "none"}>
      <button type="button" onClick={() => { props.onSort({ key: props.column, desc: active ? !props.sort.desc : props.column !== "label" }); }}>
        {props.label}
        {active && (props.sort.desc ? <ArrowDown size={12} /> : <ArrowUp size={12} />)}
      </button>
    </th>
  );
}

function Totals(props: { report: UsageReport }): JSX.Element {
  const t = props.report.totals;
  const tokens = t.input_tokens + t.output_tokens + t.cache_read_tokens + t.cache_write_tokens;
  return (
    <div class="usage-totals">
      <div class="card">
        <span class="muted small">Estimated cost</span>
        <b class="usage-big">{costLabel(t)}</b>
        {t.unknown_cost_calls > 0 && <span class="muted small">{t.unknown_cost_calls} calls with an unknown price</span>}
      </div>
      <div class="card">
        <span class="muted small">Tokens</span>
        <b class="usage-big">{count(tokens)}</b>
        <span class="muted small">
          {count(t.input_tokens)} in · {count(t.output_tokens)} out · {count(t.cache_read_tokens)} cache read ·{" "}
          {count(t.cache_write_tokens)} cache write
        </span>
      </div>
      <div class="card">
        <span class="muted small">Calls</span>
        <b class="usage-big">{t.calls.toLocaleString("en")}</b>
        {t.not_tracked_calls > 0 && <span class="muted small">{t.not_tracked_calls} not tracked</span>}
      </div>
    </div>
  );
}

function DayChart(props: { report: UsageReport; days: string[] }): JSX.Element {
  const byDay = new Map(props.report.groups.map((g) => [g.key, g]));
  const cost = (d: string): number => byDay.get(d)?.cost_usd ?? 0;
  const max = Math.max(...props.days.map(cost), 0);
  const [w, h] = [24, 120];
  return (
    <figure class="usage-chart">
      <figcaption class="muted small">Estimated cost per day · max {usd(max)}</figcaption>
      <svg viewBox={`0 0 ${String(props.days.length * w)} ${String(h)}`} preserveAspectRatio="none" role="img" aria-label="Estimated cost per day">
        {props.days.map((d, i) => {
          const group = byDay.get(d);
          const bar = max > 0 ? Math.max((cost(d) / max) * (h - 4), cost(d) > 0 ? 2 : 0) : 0;
          return (
            <rect key={d} x={i * w + 3} y={h - bar} width={w - 6} height={bar} rx={2}>
              <title>{`${d}: ${group ? costLabel(group) : "no calls"}`}</title>
            </rect>
          );
        })}
      </svg>
      <div class="usage-chart-axis muted small">
        <span>{props.days[0]}</span>
        <span>{props.days[props.days.length - 1]}</span>
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
          {props.editable && <span class="muted small"> · {g.key.slice(0, g.key.indexOf("/"))}</span>}
        </td>
        <td class="num">
          {g.calls.toLocaleString("en")}
          {g.not_tracked_calls > 0 && <span class="muted small"> · {g.not_tracked_calls} not tracked</span>}
        </td>
        <td class="num">{count(g.input_tokens)}</td>
        <td class="num">{count(g.output_tokens)}</td>
        <td class="num">{count(g.cache_read_tokens)}</td>
        <td class="num">{count(g.cache_write_tokens)}</td>
        <td class={`num${g.cost_usd == null ? " muted" : ""}`}>{costLabel(g)}</td>
        {props.editable && (
          <td>
            <IconButton label={`Edit the price of ${g.label}`} pressed={props.editing} onClick={props.onEdit}>
              <Pencil size={14} />
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
function PriceForm(props: { modelKey: string; prices: PriceEntry[]; onSaved: () => void }): JSX.Element {
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
  const valid = [values.input, values.output].every((v) => v.trim() !== "" && Number(v) >= 0) &&
    [values.cache_read, values.cache_write].every((v) => v.trim() === "" || Number(v) >= 0);

  const save = (): void => {
    const others = props.prices
      .filter((p) => p.source == null && !(p.provider_kind === kind && p.model === model))
      .map(({ provider_kind, model: m, input, output, cache_read, cache_write }) => ({ provider_kind, model: m, input, output, cache_read, cache_write }));
    void run(() =>
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
    ).then((saved) => { if (saved) props.onSaved(); });
  };

  return (
    <form class="usage-price" onSubmit={(e) => { e.preventDefault(); if (valid) save(); }}>
      <span class="muted small">
        USD per 1M tokens for {model}
        {current?.source ? ` · now from ${new URL(current.source).host}, checked ${current.checked ?? "?"}` : current ? " · your price" : " · no price yet"}
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
      <Button small type="submit" disabled={!valid}>Save price</Button>
    </form>
  );
}
