import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Pencil, Play, Plus, Trash2, X } from "lucide-preact";

import type { Agent, Flow, FlowRun, Step } from "../api.ts";
import { useApp } from "../context.ts";
import { RUN_LABELS, blankFlow, newStep, flowStepLabel, upsertRun } from "../lib/flows.ts";
import { relativeTime } from "../lib/format.ts";
import { TraceDialog } from "./Trace.tsx";
import { Button, Dialog, IconButton } from "./ui.tsx";

const MAX_STEPS = 10;

/** Flows (proof of concept): steps set up once, run in order from here. */
export function FlowsSettings(): JSX.Element | null {
  const { client, run, agents, listen } = useApp();
  const [flows, setFlows] = useState<Flow[] | null>(null);
  const [runs, setRuns] = useState<FlowRun[]>([]);
  const [editing, setEditing] = useState<Flow | null>(null);
  const [trace, setTrace] = useState<{ agent: Agent; turn: string } | null>(null);

  useEffect(() => {
    void run(async () => {
      const [loaded, recent] = await Promise.all([client.flows(), client.flowRuns()]);
      setFlows(loaded);
      setRuns(recent);
    });
    return listen((e) => {
      if (e.type === "flow_run") setRuns((rs) => upsertRun(rs, e.run));
    });
  }, []);

  if (!flows) return null;
  const nameOf = (id: string): string => agents.get(id)?.name ?? "a deleted agent";
  const list = [...agents.values()];
  const save = async (next: Flow[]): Promise<boolean> => {
    const saved = await run(() => client.setFlows(next));
    if (saved) setFlows(saved);
    return saved !== undefined;
  };

  return (
    <section class="settings-section">
      <div class="hook-section">
        <header class="hook-head">
          <div>
            <p class="pref-sub">
              Steps you set up once and run in order: an agent’s turn, a condition, a message. Each
              agent step is that agent’s own turn, with its tools and cards. A proof of concept:
              flows start from here.
            </p>
          </div>
          <Button
            small
            variant="primary"
            disabled={list.length === 0}
            onClick={() => {
              setEditing(blankFlow(list[0]?.id ?? ""));
            }}
          >
            <Plus size={16} /> New flow
          </Button>
        </header>
        {flows.length === 0 ? (
          <p class="muted small">No flows yet.</p>
        ) : (
          <ul class="list">
            {flows.map((f) => (
              <li key={f.id}>
                <div class="grow">
                  <strong>{f.name}</strong>
                  <span class="muted small ellipsis">
                    {f.steps.map((s) => flowStepLabel(s, nameOf)).join(" → ")}
                  </span>
                </div>
                <IconButton label="Run" onClick={() => void run(() => client.runFlow(f.id))}>
                  <Play size={16} />
                </IconButton>
                <IconButton
                  label="Edit"
                  onClick={() => {
                    setEditing(f);
                  }}
                >
                  <Pencil size={16} />
                </IconButton>
                <IconButton
                  label="Delete"
                  tone="danger"
                  onClick={() => void save(flows.filter((x) => x.id !== f.id))}
                >
                  <Trash2 size={16} />
                </IconButton>
              </li>
            ))}
          </ul>
        )}
      </div>

      <div class="hook-section">
        <h3 class="pref-head">Runs</h3>
        {runs.length === 0 ? (
          <p class="muted small">No runs since the daemon started.</p>
        ) : (
          <ul class="list flow-runs">
            {runs.map((r) => (
              <RunRow
                key={r.id}
                run={r}
                nameOf={nameOf}
                onTrace={(agent, turn) => {
                  const a = agents.get(agent);
                  if (a) setTrace({ agent: a, turn });
                }}
              />
            ))}
          </ul>
        )}
      </div>

      {editing && (
        <FlowDialog
          flow={editing}
          agents={list}
          onClose={() => {
            setEditing(null);
          }}
          onSave={async (f) => {
            const next = flows.some((x) => x.id === f.id)
              ? flows.map((x) => (x.id === f.id ? f : x))
              : [...flows, f];
            if (await save(next)) setEditing(null);
          }}
        />
      )}
      {trace && (
        <TraceDialog
          agent={trace.agent}
          turn={trace.turn}
          onClose={() => {
            setTrace(null);
          }}
        />
      )}
    </section>
  );
}

function RunRow(props: {
  run: FlowRun;
  nameOf: (id: string) => string;
  onTrace: (agent: string, turn: string) => void;
}): JSX.Element {
  const r = props.run;
  return (
    <li class="flow-run">
      <details>
        <summary>
          <span class={`run-status run-${r.status}`}>{RUN_LABELS[r.status]}</span>
          <strong class="grow ellipsis">{r.flow_name}</strong>
          <span class="muted small">{relativeTime(r.started_at)}</span>
        </summary>
        {r.reason && <p class="muted small">{r.reason}</p>}
        <ol class="run-steps">
          {r.plan.map((step, i) => {
            const s = r.steps[i];
            const turn = s?.turn_id;
            return (
              <li key={i}>
                <div class="run-step-head">
                  <span class={`run-status run-${s?.status ?? "waiting"}`}>
                    {RUN_LABELS[s?.status ?? "waiting"]}
                  </span>
                  <span class="grow ellipsis">{flowStepLabel(step, props.nameOf)}</span>
                  {step.kind === "agent" && turn && (
                    <Button
                      small
                      variant="ghost"
                      onClick={() => {
                        props.onTrace(step.agent, turn);
                      }}
                    >
                      Trace
                    </Button>
                  )}
                </div>
                {s?.output && <pre class="run-output">{s.output}</pre>}
              </li>
            );
          })}
        </ol>
      </details>
    </li>
  );
}

function FlowDialog(props: {
  flow: Flow;
  agents: Agent[];
  onClose: () => void;
  onSave: (flow: Flow) => Promise<void>;
}): JSX.Element {
  const [name, setName] = useState(props.flow.name);
  const [steps, setSteps] = useState<Step[]>(props.flow.steps);
  const first = props.agents[0]?.id ?? "";
  const set = (i: number, step: Step): void => {
    setSteps((ss) => ss.map((s, j) => (j === i ? step : s)));
  };
  return (
    <Dialog
      title={props.flow.name ? `Edit ${props.flow.name}` : "New flow"}
      wide
      onClose={props.onClose}
      footer={
        <>
          <Button variant="ghost" onClick={props.onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            disabled={!name.trim() || steps.length === 0}
            onClick={() => void props.onSave({ ...props.flow, name: name.trim(), steps })}
          >
            Save
          </Button>
        </>
      }
    >
      <label class="field">
        Name
        <input
          value={name}
          maxLength={60}
          placeholder="Ship a release"
          onInput={(e) => {
            setName(e.currentTarget.value);
          }}
        />
      </label>
      <ol class="flow-steps">
        {steps.map((s, i) => (
          <li key={i} class="flow-step">
            <div class="flow-step-head">
              <select
                aria-label="Kind of step"
                value={s.kind}
                onChange={(e) => {
                  set(i, newStep(e.currentTarget.value as Step["kind"], first));
                }}
              >
                <option value="agent">Agent turn</option>
                <option value="condition">Condition</option>
                <option value="message">Message</option>
              </select>
              {s.kind !== "condition" && (
                <select
                  aria-label="Agent"
                  value={s.agent}
                  onChange={(e) => {
                    set(i, { ...s, agent: e.currentTarget.value });
                  }}
                >
                  {props.agents.map((a) => (
                    <option key={a.id} value={a.id}>
                      {a.name}
                    </option>
                  ))}
                </select>
              )}
              <span class="spacer" />
              <IconButton
                label="Remove step"
                onClick={() => {
                  setSteps((ss) => ss.filter((_, j) => j !== i));
                }}
              >
                <X size={16} />
              </IconButton>
            </div>
            {s.kind === "agent" && (
              <textarea
                aria-label="Prompt"
                rows={2}
                placeholder="What the agent should do; it also gets the previous step’s output"
                value={s.prompt}
                onInput={(e) => {
                  set(i, { ...s, prompt: e.currentTarget.value });
                }}
              />
            )}
            {s.kind === "condition" && (
              <input
                aria-label="Contains"
                placeholder="Goes on only if the previous output contains…"
                value={s.contains}
                onInput={(e) => {
                  set(i, { ...s, contains: e.currentTarget.value });
                }}
              />
            )}
            {s.kind === "message" && (
              <p class="muted small">Sends the previous output to this agent, without waiting.</p>
            )}
          </li>
        ))}
      </ol>
      {steps.length < MAX_STEPS && (
        <Button
          small
          onClick={() => {
            setSteps((ss) => [...ss, newStep("agent", first)]);
          }}
        >
          <Plus size={16} /> Add step
        </Button>
      )}
    </Dialog>
  );
}
