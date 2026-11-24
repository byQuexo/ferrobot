import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { ArrowLeft, Brain, Hourglass, Send, Wrench } from "lucide-preact";

import type { Agent, Trace, TurnLink } from "../api.ts";
import { useApp } from "../context.ts";
import {
  layoutTrace,
  traceDuration,
  traceLabel,
  waitStatus,
  type SpanKind,
  type TraceRow,
} from "../lib/trace.ts";
import { Avatar } from "./Avatar.tsx";
import { Dialog } from "./ui.tsx";

/** How often a running turn's trace is fetched again. */
const REFRESH_MS = 1500;

const ICONS: Record<SpanKind, typeof Brain> = {
  model: Brain,
  tool: Wrench,
  wait: Hourglass,
  handoff: Send,
};

/** One turn on a timeline: its model and tool calls, how long each took, and what failed.
 * Handoffs open the other agent's turn; Back returns. */
export function TraceDialog(props: {
  agent: Agent;
  turn: string;
  onClose: () => void;
}): JSX.Element {
  const { client, nameOf, agents } = useApp();
  const [path, setPath] = useState([{ agent: props.agent, turn: props.turn }]);
  const here = path[path.length - 1] ?? { agent: props.agent, turn: props.turn };
  const [trace, setTrace] = useState<Trace | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    let live = true;
    let timer: number | undefined;
    setTrace(null);
    setError(null);
    const load = (): void => {
      client.trace(here.agent.id, here.turn).then(
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
  }, [here.agent.id, here.turn]);

  /** The other agent's turn, when it still exists and took the message. */
  const target = (link: TurnLink | null | undefined): { agent: Agent; turn: string } | null => {
    const agent = link ? agents.get(link.agent_id) : undefined;
    return agent && link?.turn_id ? { agent, turn: link.turn_id } : null;
  };
  const open = (to: { agent: Agent; turn: string }): void => {
    setQuery("");
    setPath([...path, to]);
  };
  const label = (r: TraceRow): string => traceLabel(r.kind, r.name, r.input, nameOf, r.link);
  const layout = trace ? layoutTrace(trace, now) : null;
  const q = query.trim().toLowerCase();
  const rows = layout?.rows.filter((r) => !q || label(r).toLowerCase().includes(q)) ?? [];
  const back = path[path.length - 2];
  const from = target(trace?.from);
  const fromName = trace?.from && (nameOf(trace.from.agent_id) ?? trace.from.agent_name);
  return (
    <Dialog title="Trace" wide onClose={props.onClose}>
      {back && (
        <button
          type="button"
          class="text-link trace-back"
          onClick={() => {
            setQuery("");
            setPath(path.slice(0, -1));
          }}
        >
          <ArrowLeft size={14} /> Back to @{back.agent.name}'s turn
        </button>
      )}
      {error && <p class="block-error">{error}</p>}
      {trace && layout && (
        <div class="trace">
          {fromName && (
            <p class="muted small trace-from">
              Started by a message from{" "}
              {from ? (
                <button
                  type="button"
                  class="text-link"
                  onClick={() => {
                    open(from);
                  }}
                >
                  @{fromName}'s turn
                </button>
              ) : (
                `@${fromName}`
              )}
            </p>
          )}
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
                <Avatar agent={here.agent} size={20} />
                {here.agent.name}
              </span>
              <span class="trace-lane">
                <span
                  class={`trace-bar root${trace.outcome === "failed" ? " failed" : ""}`}
                  style={{ left: "0%", width: "100%" }}
                />
              </span>
            </div>
            {rows.map((r) => {
              const Icon = ICONS[r.kind];
              const to = r.kind === "handoff" ? target(r.link) : null;
              return (
                <div key={r.index} class="trace-row" title={r.error ?? undefined}>
                  <span class="trace-label">
                    <Icon size={14} />
                    {to ? (
                      <button
                        type="button"
                        class="text-link trace-name"
                        title={`Open @${to.agent.name}'s turn`}
                        onClick={() => {
                          open(to);
                        }}
                      >
                        {label(r)}
                      </button>
                    ) : (
                      <span class="trace-name">{label(r)}</span>
                    )}
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
                      {r.kind !== "handoff"
                        ? traceDuration(r.ms)
                        : !r.link?.turn_id && agents.has(r.link?.agent_id ?? "")
                          ? "not taken yet"
                          : null}
                      {r.kind === "wait" && ` · ${waitStatus(r.outcome, r.running)}`}
                      {r.slowest && <span class="trace-slowest">Slowest</span>}
                    </span>
                  </span>
                </div>
              );
            })}
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
              <i class="trace-key wait" /> Waiting for you
            </span>
            <span>
              <i class="trace-key handoff" /> Handoff
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
