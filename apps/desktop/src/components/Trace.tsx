import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Brain, Wrench } from "lucide-preact";

import type { Agent, Trace } from "../api.ts";
import { useApp } from "../context.ts";
import { layoutTrace, traceDuration, traceLabel, type TraceRow } from "../lib/trace.ts";
import { Avatar } from "./Avatar.tsx";
import { Dialog } from "./ui.tsx";

/** How often a running turn's trace is fetched again. */
const REFRESH_MS = 1500;

/** One turn on a timeline: its model and tool calls, how long each took, and what failed. */
export function TraceDialog(props: {
  agent: Agent;
  turn: string;
  onClose: () => void;
}): JSX.Element {
  const { client, nameOf } = useApp();
  const [trace, setTrace] = useState<Trace | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    let live = true;
    let timer: number | undefined;
    const load = (): void => {
      client.trace(props.agent.id, props.turn).then(
        (t) => {
          if (!live) return;
          setTrace(t);
          setNow(Date.now());
          if (t.ended_at == null) timer = window.setTimeout(load, REFRESH_MS);
        },
        (err: unknown) => {
          if (live) setError(err instanceof Error ? err.message : String(err));
        },
      );
    };
    load();
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [props.agent.id, props.turn]);

  const label = (r: TraceRow): string => traceLabel(r.kind, r.name, r.input, nameOf);
  const layout = trace ? layoutTrace(trace, now) : null;
  const q = query.trim().toLowerCase();
  const rows = layout?.rows.filter((r) => !q || label(r).toLowerCase().includes(q)) ?? [];
  return (
    <Dialog title="Trace" wide onClose={props.onClose}>
      {error && <p class="block-error">{error}</p>}
      {trace && layout && (
        <div class="trace">
          <div class="trace-stats">
            <Stat
              label="Duration"
              value={`${traceDuration(layout.ms)}${layout.running ? "…" : ""}`}
            />
            <Stat
              label="Model calls"
              value={trace.models_tracked ? String(layout.models) : "not tracked"}
            />
            <Stat label="Tool calls" value={String(layout.tools)} />
            <Stat label="Errors" value={String(layout.errors)} />
          </div>
          <input
            class="trace-search"
            type="search"
            placeholder="Search steps"
            aria-label="Search steps"
            value={query}
            onInput={(e) => {
              setQuery(e.currentTarget.value);
            }}
          />
          <div class="trace-grid">
            <div class="trace-head">
              <span>Steps</span>
              <span class="trace-ticks">
                <span>0s</span>
                <span>{traceDuration(layout.ms / 2)}</span>
                <span>{traceDuration(layout.ms)}</span>
              </span>
            </div>
            <div class="trace-row trace-root">
              <span class="trace-label">
                <Avatar agent={props.agent} size={20} />
                {props.agent.name}
              </span>
              <span class="trace-lane">
                <span
                  class={`trace-bar root${trace.outcome === "failed" ? " failed" : ""}`}
                  style={{ left: "0%", width: "100%" }}
                />
              </span>
            </div>
            {rows.map((r) => (
              <div key={r.index} class="trace-row" title={r.error ?? undefined}>
                <span class="trace-label">
                  {r.kind === "model" ? <Brain size={14} /> : <Wrench size={14} />}
                  <span class="trace-name">{label(r)}</span>
                </span>
                <span class="trace-lane">
                  <span
                    class={`trace-bar ${r.kind}${r.error ? " failed" : ""}${r.running ? " running" : ""}`}
                    style={{ left: `${String(r.left)}%`, width: `${String(r.width)}%` }}
                  />
                  <span
                    class={`trace-time ${r.labelAt}`}
                    style={
                      r.labelAt === "after"
                        ? { left: `calc(${String(r.left + r.width)}% + 6px)` }
                        : r.labelAt === "before"
                          ? { right: `calc(${String(100 - r.left)}% + 6px)` }
                          : { right: `calc(${String(100 - r.left - r.width)}% + 6px)` }
                    }
                  >
                    {traceDuration(r.ms)}
                    {r.slowest && <span class="trace-slowest">Slowest</span>}
                  </span>
                </span>
              </div>
            ))}
            {rows.length === 0 && <p class="muted trace-empty">No steps{q ? " match" : ""}.</p>}
          </div>
          <div class="trace-legend">
            <span>
              <i class="trace-key model" /> Model call
            </span>
            <span>
              <i class="trace-key tool" /> Tool call
            </span>
            <span>
              <i class="trace-key failed" /> Failed
            </span>
          </div>
          {!trace.models_tracked && (
            <p class="muted small">
              Claude Code calls the model from your own login, so its model calls are not in the
              trace.
            </p>
          )}
          {trace.error && <p class="block-error">{trace.error}</p>}
        </div>
      )}
    </Dialog>
  );
}

function Stat(props: { label: string; value: string }): JSX.Element {
  return (
    <div class="trace-stat">
      <span class="muted small">{props.label}</span>
      <strong>{props.value}</strong>
    </div>
  );
}
