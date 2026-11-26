import { Fragment, type JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import {
  Activity,
  Brain,
  CalendarClock,
  Camera,
  Cpu,
  History,
  Moon,
  Play,
  Plus,
  Power,
  RotateCcw,
  Search,
  Trash2,
  UserRound,
} from "lucide-preact";

import type {
  Agent,
  MemoryEntry,
  Provider,
  Routine,
  RoutineRun,
  SandboxInfo,
  Snapshot,
  StoredEvent,
} from "../api.ts";
import { useApp } from "../context.ts";
import {
  computerSummary,
  dateTime,
  duration,
  presenceLabel,
  relativeTime,
  statusOf,
} from "../lib/format.ts";
import { buildTimeline, type TimelineItem } from "../lib/timeline.ts";
import { modelLabel } from "../lib/models.ts";
import { TagChips } from "./AgentMeta.tsx";
import { Avatar } from "./Avatar.tsx";
import { Console, Files } from "./ComputerTools.tsx";
import { DesktopPreview } from "./Desktop.tsx";
import { CredentialFix } from "./CredentialFix.tsx";
import { ToolCard } from "./Chat.tsx";
import { AgentWebhooks } from "./WebhooksSettings.tsx";
import { KIND_LABELS } from "./ProvidersSettings.tsx";
import { Button, Empty, IconButton, Skeleton, Switch } from "./ui.tsx";

type Tab = "profile" | "computer" | "routines" | "memory" | "activities";

const TABS: [Tab, string, typeof UserRound][] = [
  ["profile", "Profile", UserRound],
  ["computer", "Computer", Cpu],
  ["routines", "Routines", CalendarClock],
  ["memory", "Memory", Brain],
  ["activities", "Activities", Activity],
];

export function InfoPanel(props: {
  agent: Agent;
  tab: Tab;
  onTab: (t: Tab) => void;
  onEdit: () => void;
  onVault: () => void;
  onDeleted: () => void;
}): JSX.Element {
  return (
    <aside class="info" aria-label="Agent details">
      <div class="tabs" role="tablist">
        {TABS.map(([t, label, Icon]) => (
          <button
            key={t}
            type="button"
            role="tab"
            aria-selected={props.tab === t}
            class={props.tab === t ? "active" : ""}
            onClick={() => {
              props.onTab(t);
            }}
          >
            <Icon size={16} />
            <span>{label}</span>
          </button>
        ))}
      </div>
      <div class="info-body">
        {props.tab === "profile" && (
          <Profile
            agent={props.agent}
            onEdit={props.onEdit}
            onVault={props.onVault}
            onDeleted={props.onDeleted}
          />
        )}
        {props.tab === "computer" && <Computer agent={props.agent} />}
        {props.tab === "routines" && <Routines agent={props.agent} />}
        {props.tab === "memory" && <Memory agent={props.agent} />}
        {props.tab === "activities" && <Activities agent={props.agent} />}
      </div>
    </aside>
  );
}
export type { Tab as InfoTab };

/** Long instructions fold to their first lines, so the cards below stay in view. */
function Instructions(props: { text: string }): JSX.Element {
  const [open, setOpen] = useState(false);
  const text = props.text.trim();
  const long = text.length > 320 || text.split("\n").length > 6;
  return (
    <section class="card">
      <h3>Instructions</h3>
      {text ? (
        <>
          <p class={`prewrap${long && !open ? " folded" : ""}`}>{text}</p>
          {long && (
            <button
              type="button"
              class="text-link small"
              aria-expanded={open}
              onClick={() => {
                setOpen(!open);
              }}
            >
              {open ? "Show less" : "Show all"}
            </button>
          )}
        </>
      ) : (
        <p class="muted">None. The agent uses its defaults.</p>
      )}
    </section>
  );
}

function Profile(props: {
  agent: Agent;
  onEdit: () => void;
  onVault: () => void;
  onDeleted: () => void;
}): JSX.Element {
  const { client, run, agents, health } = useApp();
  const a = props.agent;
  const status = statusOf(a);
  const problem = a.credential_error ?? a.tools_unsupported;
  const creator = a.created_by ? agents.get(a.created_by) : undefined;
  const [confirm, setConfirm] = useState(false);
  const [providers, setProviders] = useState<Provider[] | null>(null);
  useEffect(() => {
    client.listProviders().then(setProviders, () => {
      setProviders([]);
    });
  }, [a.provider_id]);
  const provider = providers?.find((p) => p.id === a.provider_id);
  const model = modelLabel(a, provider);
  return (
    <div class="profile">
      <Avatar agent={a} size={88} />
      <h2>{a.name}</h2>
      <p class="muted center">{a.description || "No description yet."}</p>
      <TagChips tags={a.tags} />
      <div class="kv">
        <span>Status</span>
        <span class="model-line">
          <span class={`status-chip p-${status.presence}`}>{status.label}</span>
          {problem && (
            <>
              <span class="muted">{problem.reason}</span>
              <CredentialFix agent={a} onEdit={props.onEdit} onVault={props.onVault} />
            </>
          )}
        </span>
        <span>Model</span>
        <span class="model-line">
          <span class="mono">{model}</span>
          <span class="muted">
            ·{" "}
            {a.provider_id
              ? provider
                ? `${provider.name} (${KIND_LABELS[provider.kind]})`
                : "…"
              : health?.anthropic_key === false
                ? "Anthropic (daemon key, not set up: pick a provider)"
                : "Anthropic (daemon key)"}
          </span>
          <button type="button" class="text-link" onClick={props.onEdit}>
            Change
          </button>
        </span>
        <span>Created</span>
        <span>{dateTime(a.created_at)}</span>
        {a.created_by && (
          <>
            <span>Created by</span>
            {creator ? (
              <span class="created-by">
                <Avatar agent={creator} size={18} />
                <b>{creator.name}</b>
              </span>
            ) : (
              <span class="muted">a deleted agent</span>
            )}
          </>
        )}
      </div>
      <Instructions text={a.instructions} />
      <AgentWebhooks agent={a.id} />
      <div class="row-actions">
        <Button variant="primary" onClick={props.onEdit}>
          Edit profile
        </Button>
        {confirm ? (
          <Button
            variant="danger"
            onClick={() =>
              void run(async () => {
                await client.deleteAgent(a.id);
                props.onDeleted();
              })
            }
          >
            Delete for good
          </Button>
        ) : (
          <Button
            variant="ghost"
            onClick={() => {
              setConfirm(true);
            }}
          >
            <Trash2 size={16} /> Delete agent
          </Button>
        )}
      </div>
      {confirm && (
        <p class="muted small">This removes the agent, its computer, chat, memory and routines.</p>
      )}
    </div>
  );
}

function Computer(props: { agent: Agent }): JSX.Element {
  const { client, run, listen } = useApp();
  const id = props.agent.id;
  const [info, setInfo] = useState<SandboxInfo | null>(null);
  const [snaps, setSnaps] = useState<Snapshot[]>([]);
  const [busy, setBusy] = useState(false);
  const [label, setLabel] = useState("");

  const refresh = async (): Promise<void> => {
    const [i, s] = await Promise.all([client.sandbox(id), client.snapshots(id)]);
    setInfo(i);
    setSnaps(s);
  };
  useEffect(() => {
    void run(refresh);
    const timer = setInterval(() => void refresh().catch(() => undefined), 10_000);
    const off = listen((e) => {
      if (e.type === "agent" && e.agent.id === id) void refresh().catch(() => undefined);
    });
    return () => {
      clearInterval(timer);
      off();
    };
  }, [id, props.agent.status]);

  const act = async (fn: () => Promise<unknown>): Promise<void> => {
    setBusy(true);
    await run(async () => {
      await fn();
      await refresh();
    });
    setBusy(false);
  };

  if (!info) return <Skeleton lines={5} />;
  const paused = props.agent.status === "paused";
  const archived = props.agent.status === "archived";
  const asleep = paused || archived || props.agent.status === "stopped";
  return (
    <div class="stack">
      <section class="card computer">
        <div class={`machine p-${info.presence}`}>
          <div class="grow">
            <strong class="ellipsis" title={`${props.agent.name}'s computer`}>
              {props.agent.name}'s computer
            </strong>
            <span class="muted small">
              {asleep
                ? computerSummary(props.agent.status, info)
                : `${presenceLabel(info.presence)} · ${computerSummary(props.agent.status, info)}`}
            </span>
          </div>
          <DesktopPreview
            key={`desktop-${id}`}
            agent={id}
            name={props.agent.name}
            asleep={asleep}
          />
        </div>
        <details class="computer-details">
          <summary>Details</summary>
          <div class="kv">
            <span>Container</span>
            <span>
              {info.state ?? "none"}
              {info.connected ? " · runner connected" : ""}
            </span>
            <span>Backend</span>
            <span>{info.backend ?? "—"}</span>
            <span>Sandbox</span>
            <span class="mono ellipsis" title={info.sandbox_id ?? ""}>
              {info.sandbox_id ?? "—"}
            </span>
            <span>Idle</span>
            <span>
              {paused
                ? "paused, resumes quickly"
                : archived
                  ? "archived, restores on wake"
                  : asleep
                    ? "asleep"
                    : duration(info.idle_s)}
              {!asleep && info.warm_after_s ? ` · pauses after ${duration(info.warm_after_s)}` : ""}
              {!paused && !asleep && info.idle_timeout_s
                ? ` · sleeps after ${duration(info.idle_timeout_s)}`
                : ""}
              {paused && info.idle_timeout_s
                ? ` · sleeps after ${duration(info.idle_timeout_s)} idle`
                : ""}
            </span>
          </div>
        </details>
        <div class="row-actions">
          {asleep ? (
            <Button
              variant="primary"
              disabled={busy}
              onClick={() => void act(() => client.wake(id))}
            >
              <Power size={16} /> Wake up
            </Button>
          ) : (
            <Button disabled={busy} onClick={() => void act(() => client.sleep(id))}>
              <Moon size={16} /> Put to sleep
            </Button>
          )}
        </div>
      </section>

      {asleep ? (
        <p class="muted small">Wake the computer to browse its files and run commands.</p>
      ) : (
        <>
          <Files key={`files-${id}`} agent={id} />
          <Console key={`console-${id}`} agent={id} />
        </>
      )}

      <section class="card">
        <h3>
          <History size={16} /> Snapshots
        </h3>
        {info.snapshots_supported ? (
          <>
            <form
              class="inline-form"
              onSubmit={(e) => {
                e.preventDefault();
                void act(async () => {
                  await client.snapshot(id, label.trim() || "manual");
                  setLabel("");
                });
              }}
            >
              <input
                placeholder="Label (optional)"
                value={label}
                maxLength={64}
                onInput={(e) => {
                  setLabel(e.currentTarget.value);
                }}
              />
              <Button type="submit" small disabled={busy}>
                <Camera size={16} /> Take
              </Button>
            </form>
            {snaps.length === 0 && (
              <p class="muted small">
                No snapshots yet. A snapshot saves the whole computer, including files.
              </p>
            )}
            <ul class="list">
              {snaps.map((s) => (
                <li key={s.id}>
                  <div class="grow">
                    <strong>{s.label}</strong>
                    <span class="muted small">{dateTime(s.created_at)}</span>
                  </div>
                  <IconButton
                    label="Restore this snapshot"
                    disabled={busy}
                    onClick={() => void act(() => client.restore(id, s.id))}
                  >
                    <RotateCcw size={16} />
                  </IconButton>
                  <IconButton
                    label="Delete snapshot"
                    tone="danger"
                    disabled={busy}
                    onClick={() => void act(() => client.deleteSnapshot(id, s.id))}
                  >
                    <Trash2 size={16} />
                  </IconButton>
                </li>
              ))}
            </ul>
          </>
        ) : (
          <p class="muted small">This sandbox backend does not support snapshots.</p>
        )}
      </section>
    </div>
  );
}

const PRESETS: [string, string][] = [
  ["0 9 * * *", "Every day at 09:00"],
  ["0 9 * * 1-5", "Weekdays at 09:00"],
  ["@hourly", "Every hour"],
  ["0 18 * * 5", "Fridays at 18:00"],
];

function Routines(props: { agent: Agent }): JSX.Element {
  const { client, run, listen, health } = useApp();
  const id = props.agent.id;
  const [routines, setRoutines] = useState<Routine[] | null>(null);
  const [adding, setAdding] = useState(false);
  const [open, setOpen] = useState<number | null>(null);

  const refresh = async (): Promise<void> => {
    setRoutines(await client.routines(id));
  };
  useEffect(() => {
    void run(refresh);
    return listen((e) => {
      if (e.type === "routines_changed" && e.agent_id === id) void refresh().catch(() => undefined);
    });
  }, [id]);

  if (!routines) return <Skeleton lines={4} />;
  return (
    <div class="stack">
      {routines.length === 0 && !adding && (
        <Empty icon={<CalendarClock size={28} />} title="No routines">
          Routines send {props.agent.name} a prompt on a schedule, waking it if it sleeps.
        </Empty>
      )}
      {routines.map((r) => (
        <section key={r.id} class={`card routine${r.enabled ? "" : " disabled"}`}>
          <div class="routine-head">
            <button
              type="button"
              class="link grow"
              onClick={() => {
                setOpen(open === r.id ? null : r.id);
              }}
            >
              <strong>{r.name}</strong>
              <span class="muted small">{r.schedule_text || r.schedule}</span>
            </button>
            <Switch
              checked={r.enabled}
              label={r.enabled ? "Disable routine" : "Enable routine"}
              onChange={(v) => void run(() => client.updateRoutine(r.id, { enabled: v }))}
            />
          </div>
          <div class="routine-meta small muted">
            {r.enabled && r.next_run_at ? `Next ${relativeTime(r.next_run_at)}` : "Paused"}
            {r.last_run_at ? ` · last ${relativeTime(r.last_run_at)}` : ""}
            {r.timezone !== "UTC" ? ` · ${r.timezone}` : ""}
          </div>
          {open === r.id && <RoutineDetail routine={r} />}
          <div class="row-actions">
            <Button small onClick={() => void run(() => client.runRoutine(r.id))}>
              <Play size={16} /> Run now
            </Button>
            <span class="spacer" />
            <IconButton
              label="Delete routine"
              tone="danger"
              onClick={() => void run(() => client.deleteRoutine(r.id))}
            >
              <Trash2 size={16} />
            </IconButton>
          </div>
        </section>
      ))}
      {adding ? (
        <RoutineForm
          timezone={health?.timezone ?? "UTC"}
          onCancel={() => {
            setAdding(false);
          }}
          onSave={async (req) => {
            const ok = await run(() => client.createRoutine(id, req));
            if (ok) setAdding(false);
          }}
        />
      ) : (
        <Button
          onClick={() => {
            setAdding(true);
          }}
        >
          <Plus size={16} /> New routine
        </Button>
      )}
    </div>
  );
}

function RoutineDetail(props: { routine: Routine }): JSX.Element {
  const { client } = useApp();
  const [runs, setRuns] = useState<RoutineRun[]>([]);
  useEffect(() => {
    void client.routineRuns(props.routine.id).then(setRuns, () => undefined);
  }, [props.routine.id, props.routine.last_run_at]);
  return (
    <div class="routine-detail">
      <div class="tool-label">Prompt</div>
      <p class="prewrap">{props.routine.prompt}</p>
      <div class="tool-label">
        Schedule <span class="mono">{props.routine.schedule}</span>
      </div>
      <div class="tool-label">Recent runs</div>
      {runs.length === 0 ? (
        <p class="muted small">Not run yet.</p>
      ) : (
        <ul class="runs">
          {runs.slice(0, 5).map((run) => (
            <li key={run.id}>
              <span>{dateTime(run.created_at)}</span>
              <span class="muted">{run.trigger === "manual" ? "manual" : "scheduled"}</span>
              <span class={`run-status s-${run.status ?? "unknown"}`}>{run.status ?? "—"}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function RoutineForm(props: {
  timezone: string;
  onCancel: () => void;
  onSave: (req: {
    name: string;
    schedule: string;
    prompt: string;
    timezone: string;
  }) => Promise<void>;
}): JSX.Element {
  const [name, setName] = useState("");
  const [schedule, setSchedule] = useState(PRESETS[0]?.[0] ?? "@daily");
  const [prompt, setPrompt] = useState("");
  const [timezone, setTimezone] = useState(props.timezone);
  const [saving, setSaving] = useState(false);
  return (
    <form
      class="card form"
      onSubmit={(e) => {
        e.preventDefault();
        setSaving(true);
        void props
          .onSave({
            name: name.trim(),
            schedule: schedule.trim(),
            prompt,
            timezone: timezone.trim(),
          })
          .finally(() => {
            setSaving(false);
          });
      }}
    >
      <label>
        Name
        <input
          required
          maxLength={64}
          placeholder="morning-digest"
          value={name}
          onInput={(e) => {
            setName(e.currentTarget.value);
          }}
        />
      </label>
      <label>
        Schedule
        <input
          required
          class="mono"
          value={schedule}
          onInput={(e) => {
            setSchedule(e.currentTarget.value);
          }}
        />
      </label>
      <div class="chips">
        {PRESETS.map(([expr, text]) => (
          <button
            key={expr}
            type="button"
            class={`chip${schedule === expr ? " on" : ""}`}
            onClick={() => {
              setSchedule(expr);
            }}
          >
            {text}
          </button>
        ))}
      </div>
      <label>
        Time zone
        <input
          required
          value={timezone}
          onInput={(e) => {
            setTimezone(e.currentTarget.value);
          }}
        />
      </label>
      <label>
        Prompt
        <textarea
          required
          rows={3}
          placeholder="Summarize what changed since yesterday."
          value={prompt}
          onInput={(e) => {
            setPrompt(e.currentTarget.value);
          }}
        />
      </label>
      <div class="row-actions">
        <Button variant="ghost" onClick={props.onCancel}>
          Cancel
        </Button>
        <Button variant="primary" type="submit" disabled={saving}>
          Create routine
        </Button>
      </div>
    </form>
  );
}

/** An agent's memory, or the global memory when `agent` is null. */
export function Memory(props: { agent: Agent | null }): JSX.Element {
  const { client, run, listen, agents } = useApp();
  const id = props.agent?.id ?? null;
  const [q, setQ] = useState("");
  const [entries, setEntries] = useState<MemoryEntry[] | null>(null);
  const [adding, setAdding] = useState(false);
  const [key, setKey] = useState("");
  const [content, setContent] = useState("");

  const refresh = async (query = q): Promise<void> => {
    setEntries(await client.memory(id, query));
  };
  useEffect(() => {
    void run(() => refresh(""));
    setQ("");
    return listen((e) => {
      const changed = id
        ? e.type === "memory_changed" && e.agent_id === id
        : e.type === "global_memory_changed";
      if (changed) void refresh().catch(() => undefined);
    });
  }, [id]);
  useEffect(() => {
    const t = setTimeout(() => void refresh(q).catch(() => undefined), 200);
    return () => {
      clearTimeout(t);
    };
  }, [q]);

  return (
    <div class="stack">
      <div class="inline-form">
        <label class="search grow">
          <Search size={16} aria-hidden="true" />
          <input
            type="search"
            placeholder="Search memory"
            value={q}
            onInput={(e) => {
              setQ(e.currentTarget.value);
            }}
          />
        </label>
        <IconButton
          label="Add memory"
          tone="accent"
          onClick={() => {
            setAdding(!adding);
          }}
        >
          <Plus size={16} />
        </IconButton>
      </div>
      {adding && (
        <form
          class="card form"
          onSubmit={(e) => {
            e.preventDefault();
            void run(async () => {
              await client.remember(id, key.trim(), content);
              setKey("");
              setContent("");
              setAdding(false);
              await refresh();
            });
          }}
        >
          <label>
            Key
            <input
              required
              maxLength={128}
              placeholder="deploy-day"
              value={key}
              onInput={(e) => {
                setKey(e.currentTarget.value);
              }}
            />
          </label>
          <label>
            Content
            <textarea
              required
              rows={3}
              value={content}
              onInput={(e) => {
                setContent(e.currentTarget.value);
              }}
            />
          </label>
          <div class="row-actions">
            <Button
              variant="ghost"
              onClick={() => {
                setAdding(false);
              }}
            >
              Cancel
            </Button>
            <Button variant="primary" type="submit">
              Save
            </Button>
          </div>
        </form>
      )}
      {entries?.length === 0 && !adding && (
        <Empty icon={<Brain size={28} />} title={q ? "Nothing found" : "No memories yet"}>
          {q
            ? "Try other words."
            : props.agent
              ? `${props.agent.name} saves notes here with its memory tools; you can add some too.`
              : "Facts every agent can recall. Agents save them here with their memory tools; you can add some too."}
        </Empty>
      )}
      {entries === null && <Skeleton lines={4} />}
      <ul class="memories">
        {entries?.map((m) => (
          <li key={m.key} class="card memory">
            <div class="memory-head">
              <strong class="mono">{m.key}</strong>
              {!props.agent && (
                <span class="muted small">
                  by {m.author ? `@${agents.get(m.author)?.name ?? "a deleted agent"}` : "you"}
                </span>
              )}
              <span class="spacer" />
              <span class="muted small">{relativeTime(m.updated_at)}</span>
              <IconButton
                label={`Forget ${m.key}`}
                tone="danger"
                onClick={() =>
                  void run(async () => {
                    await client.forget(id, m.key);
                    await refresh();
                  })
                }
              >
                <Trash2 size={16} />
              </IconButton>
            </div>
            <p class="prewrap clamp">{m.content}</p>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** Tool calls and turn errors: what the agent did, newest first. */
function Activities(props: { agent: Agent }): JSX.Element {
  const { client, run, listen, agents } = useApp();
  const id = props.agent.id;
  const [events, setEvents] = useState<StoredEvent[] | null>(null);

  useEffect(() => {
    setEvents(null);
    void run(async () => {
      setEvents(await client.allEvents(id));
    });
    return listen((e) => {
      if (e.type === "event" && e.event.agent_id === id) {
        setEvents((prev) =>
          prev && !prev.some((x) => x.id === e.event.id) ? [...prev, e.event] : prev,
        );
      }
    });
  }, [id]);

  const names = {
    agents: new Map([...agents.values()].map((a) => [a.id, a.name])),
    routines: new Map<number, string>(),
  };
  const items = buildTimeline(id, [], events ?? [], new Map(), names)
    .filter(
      (i): i is Extract<TimelineItem, { type: "tool" | "error" }> =>
        i.type === "tool" || i.type === "error",
    )
    .reverse();

  if (!events) return <Skeleton lines={4} />;
  if (items.length === 0) {
    return (
      <Empty icon={<Activity size={28} />} title="No activity yet">
        Commands, file edits, messages and other tool calls of {props.agent.name} show up here.
      </Empty>
    );
  }
  // Newest first, grouped by the turn that made the calls.
  const turnOf = (i: (typeof items)[number]): string | null => (i.type === "tool" ? i.turn : null);
  return (
    <div class="stack activities">
      {items.map((i, n) => {
        const prev = items[n - 1];
        const head = n === 0 || turnOf(i) === null || turnOf(prev ?? i) !== turnOf(i);
        return (
          <Fragment key={i.key}>
            {head && <div class="act-turn">{relativeTime(i.at)}</div>}
            {i.type === "tool" ? (
              <ToolCard item={i} />
            ) : (
              <div class="notice notice-error">{i.text}</div>
            )}
          </Fragment>
        );
      })}
    </div>
  );
}
