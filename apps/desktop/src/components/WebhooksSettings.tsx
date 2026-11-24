import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import {
  Braces,
  Copy,
  Hash,
  MessagesSquare,
  Plus,
  RefreshCw,
  Trash2,
  Webhook,
} from "lucide-preact";

import type {
  IncomingWebhookCreated,
  OutgoingWebhookView,
  WebhookEvent,
  WebhookFormat,
  WebhooksView,
} from "../api.ts";
import { useApp } from "../context.ts";
import { Button, Dialog, IconButton } from "./ui.tsx";

const EVENTS: [WebhookEvent, string][] = [
  ["reply", "Replies"],
  ["question", "Cards waiting for you"],
  ["task_done", "Finished tasks"],
  ["turn_failed", "Failed turns"],
];

const FORMATS: [WebhookFormat, string, string, string][] = [
  [
    "discord",
    "Discord",
    "https://discord.com/api/webhooks/…",
    "In Discord: the channel’s settings → Integrations → Webhooks → New Webhook → Copy Webhook URL.",
  ],
  [
    "slack",
    "Slack",
    "https://hooks.slack.com/services/…",
    "In Slack: an app with Incoming Webhooks turned on → Add New Webhook to Workspace → copy the URL.",
  ],
  [
    "json",
    "JSON",
    "https://…",
    "Any URL that takes a JSON POST, such as an n8n or Zapier webhook.",
  ],
];

const ICONS: Record<WebhookFormat, typeof Hash> = {
  discord: MessagesSquare,
  slack: Hash,
  json: Braces,
};

const eventLabel = (id: WebhookEvent): string => EVENTS.find(([e]) => e === id)?.[1] ?? id;
const JSON_FORMAT: (typeof FORMATS)[number] = [
  "json",
  "JSON",
  "https://…",
  "Any URL that takes a JSON POST, such as an n8n or Zapier webhook.",
];
const formatOf = (id: WebhookFormat): (typeof FORMATS)[number] =>
  FORMATS.find(([f]) => f === id) ?? JSON_FORMAT;
const time = (at: string): string =>
  new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

interface Draft {
  name: string;
  url: string;
  format: WebhookFormat;
  events: WebhookEvent[];
  agents: string[];
}

const EMPTY: Draft = { name: "", url: "", format: "discord", events: ["reply"], agents: [] };

const toggle = <T,>(list: T[], item: T): T[] =>
  list.includes(item) ? list.filter((x) => x !== item) : [...list, item];

/** Incoming webhooks (a POST becomes a message) and outgoing ones (events go to a URL). */
export function WebhooksSettings(props: { onGuide: () => void }): JSX.Element | null {
  const { client, run, agents } = useApp();
  const [view, setView] = useState<WebhooksView | null>(null);
  const [adding, setAdding] = useState<"incoming" | "outgoing" | null>(null);
  const [shown, setShown] = useState<IncomingWebhookCreated | null>(null);
  const [renewing, setRenewing] = useState<number | null>(null);

  const load = async (): Promise<void> => {
    const loaded = await run(() => client.webhooks());
    if (loaded) setView(loaded);
  };
  useEffect(() => {
    void load();
  }, []);

  if (!view) return null;
  const agentName = (id: string): string => agents.get(id)?.name ?? "a deleted agent";
  const newUrl = async (id: number): Promise<void> => {
    setRenewing(null);
    const created = await run(() => client.regenerateIncomingWebhook(id));
    if (created) setShown(created);
  };
  const remove = async (kind: "incoming" | "outgoing", id: number): Promise<void> => {
    await run(() =>
      kind === "incoming" ? client.deleteIncomingWebhook(id) : client.deleteOutgoingWebhook(id),
    );
    await load();
  };

  return (
    <section class="settings-section">
      <div class="hook-section">
        <header class="hook-head">
          <div>
            <h3 class="pref-head">Incoming</h3>
            <p class="pref-sub">
              Other services message an agent: a POST to the webhook’s URL becomes a message, marked
              as outside data, not instructions.
            </p>
          </div>
          <Button
            small
            variant="primary"
            onClick={() => {
              setAdding("incoming");
            }}
          >
            <Plus size={16} /> New
          </Button>
        </header>
        {view.incoming.length === 0 ? (
          <p class="hook-empty muted small">
            None yet. GitHub, a form, n8n or Zapier can then reach an agent.
          </p>
        ) : (
          <ul class="conn-list">
            {view.incoming.map((h) => (
              <li key={h.id} class="conn-card">
                <span class="conn-tile hook-tile" aria-hidden="true">
                  <Webhook size={18} />
                </span>
                <div class="conn-main">
                  <div class="conn-title">
                    <b>{h.name}</b>
                  </div>
                  <span class="muted small">Messages {agentName(h.agent_id)}</span>
                  {renewing === h.id && (
                    <div class="conn-next">
                      <span class="small">The old URL stops working at once.</span>
                      <Button small variant="primary" onClick={() => void newUrl(h.id)}>
                        Make a new URL
                      </Button>
                      <Button
                        small
                        variant="ghost"
                        onClick={() => {
                          setRenewing(null);
                        }}
                      >
                        Cancel
                      </Button>
                    </div>
                  )}
                </div>
                <div class="conn-actions">
                  <IconButton
                    label={`New URL for ${h.name}`}
                    onClick={() => {
                      setRenewing(h.id);
                    }}
                  >
                    <RefreshCw size={16} />
                  </IconButton>
                  <IconButton
                    label={`Delete ${h.name}`}
                    tone="danger"
                    onClick={() => void remove("incoming", h.id)}
                  >
                    <Trash2 size={16} />
                  </IconButton>
                </div>
              </li>
            ))}
          </ul>
        )}
        <p class="hint">
          The URL works on this computer; for services on the internet, expose only{" "}
          <code>/hooks/</code> through a tunnel.{" "}
          <button type="button" class="text-link" onClick={props.onGuide}>
            How
          </button>
        </p>
      </div>

      <div class="hook-section">
        <header class="hook-head">
          <div>
            <h3 class="pref-head">Outgoing</h3>
            <p class="pref-sub">
              Your agents’ events go to a Discord or Slack channel, or any URL that takes JSON.
            </p>
          </div>
          <Button
            small
            variant="primary"
            onClick={() => {
              setAdding("outgoing");
            }}
          >
            <Plus size={16} /> New
          </Button>
        </header>
        {view.outgoing.length === 0 ? (
          <p class="hook-empty muted small">
            None yet. Hear about replies, waiting cards or finished tasks where you are.
          </p>
        ) : (
          <ul class="conn-list">
            {view.outgoing.map((h) => (
              <OutgoingCard
                key={h.id}
                hook={h}
                agentName={agentName}
                onDelete={() => void remove("outgoing", h.id)}
              />
            ))}
          </ul>
        )}
      </div>

      {adding === "incoming" && (
        <IncomingDialog
          onClose={() => {
            setAdding(null);
          }}
          onCreated={(created) => {
            setAdding(null);
            setShown(created);
            void load();
          }}
        />
      )}
      {adding === "outgoing" && (
        <OutgoingDialog
          onClose={() => {
            setAdding(null);
          }}
          onAdded={() => {
            setAdding(null);
            void load();
          }}
        />
      )}
      {shown && (
        <CreatedDialog
          hook={shown}
          agentName={agentName(shown.agent_id)}
          onClose={() => {
            setShown(null);
          }}
        />
      )}
    </section>
  );
}

function OutgoingCard(props: {
  hook: OutgoingWebhookView;
  agentName: (id: string) => string;
  onDelete: () => void;
}): JSX.Element {
  const h = props.hook;
  const Icon = ICONS[h.format];
  const last = h.last_delivery;
  const status = !last
    ? { cls: "", text: "Nothing sent yet" }
    : last.error
      ? { cls: "p-error", text: `Failed ${time(last.at)}` }
      : { cls: "p-awake", text: `Delivered ${time(last.at)}` };
  return (
    <li class="conn-card">
      <span class="conn-tile hook-tile" aria-hidden="true">
        <Icon size={18} />
      </span>
      <div class="conn-main">
        <div class="conn-title">
          <b>{h.name}</b>
          <span class={`conn-status ${status.cls}`}>
            <span class="dot" aria-hidden="true" />
            {status.text}
          </span>
        </div>
        <span class="muted small">
          {formatOf(h.format)[1]} ·{" "}
          {h.agents.length ? h.agents.map(props.agentName).join(", ") : "every agent"}
        </span>
        <span class="hook-chips">
          {h.events.map((e) => (
            <span key={e} class="hook-chip">
              {eventLabel(e)}
            </span>
          ))}
        </span>
        {last?.error && <span class="small hook-error">{last.error}</span>}
      </div>
      <div class="conn-actions">
        <IconButton label={`Delete ${h.name}`} tone="danger" onClick={props.onDelete}>
          <Trash2 size={16} />
        </IconButton>
      </div>
    </li>
  );
}

function IncomingDialog(props: {
  onClose: () => void;
  onCreated: (created: IncomingWebhookCreated) => void;
}): JSX.Element {
  const { client, run, agents } = useApp();
  const team = [...agents.values()];
  const [name, setName] = useState("");
  const [agent, setAgent] = useState(team[0]?.id ?? "");
  const create = async (): Promise<void> => {
    const created = await run(() =>
      client.createIncomingWebhook({ name: name.trim(), agent_id: agent }),
    );
    if (created) props.onCreated(created);
  };
  return (
    <Dialog
      title="New incoming webhook"
      onClose={props.onClose}
      footer={
        <>
          <Button variant="ghost" onClick={props.onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!name.trim() || !agent} onClick={() => void create()}>
            Create webhook
          </Button>
        </>
      }
    >
      <div class="form">
        <label>
          Name
          <input
            value={name}
            maxLength={64}
            spellcheck={false}
            autoComplete="off"
            placeholder="e.g. github"
            onInput={(e) => {
              setName(e.currentTarget.value);
            }}
          />
          <span class="hint">Shown in the agent’s chat with each message.</span>
        </label>
        <label>
          Agent
          <select
            value={agent}
            onChange={(e) => {
              setAgent(e.currentTarget.value);
            }}
          >
            {team.map((a) => (
              <option key={a.id} value={a.id}>
                {a.name}
              </option>
            ))}
          </select>
        </label>
      </div>
    </Dialog>
  );
}

function CreatedDialog(props: {
  hook: IncomingWebhookCreated;
  agentName: string;
  onClose: () => void;
}): JSX.Element {
  const { run, toast } = useApp();
  const copy = async (text: string): Promise<void> => {
    const copied = await run(async () => {
      await navigator.clipboard.writeText(text);
      return true;
    });
    if (copied) toast("Copied.", "info");
  };
  const curl = `curl -X POST -H 'Content-Type: application/json' \\\n  -d '{"text": "Hello from curl"}' \\\n  '${props.hook.url}'`;
  return (
    <Dialog
      title={`${props.hook.name} is ready`}
      wide
      onClose={props.onClose}
      footer={
        <Button variant="primary" onClick={props.onClose}>
          Done
        </Button>
      }
    >
      <div class="form">
        <p class="hint">
          Copy the URL now: it is shown only once. Anyone with it can message {props.agentName}.
        </p>
        <div class="webhook-url">
          <code>{props.hook.url}</code>
          <Button small onClick={() => void copy(props.hook.url)}>
            <Copy size={14} /> Copy
          </Button>
        </div>
        <span class="hint">Try it from a terminal:</span>
        <pre class="code-block">
          <code>{curl}</code>
        </pre>
      </div>
    </Dialog>
  );
}

function OutgoingDialog(props: { onClose: () => void; onAdded: () => void }): JSX.Element {
  const { client, run, agents } = useApp();
  const team = [...agents.values()];
  const [draft, setDraft] = useState<Draft>(EMPTY);
  const format = formatOf(draft.format);
  const add = async (): Promise<void> => {
    const added = await run(() =>
      client.createOutgoingWebhook({ ...draft, name: draft.name.trim(), url: draft.url.trim() }),
    );
    if (added) props.onAdded();
  };
  return (
    <Dialog
      title="New outgoing webhook"
      onClose={props.onClose}
      footer={
        <>
          <Button variant="ghost" onClick={props.onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            disabled={!draft.name.trim() || !draft.url.trim() || draft.events.length === 0}
            onClick={() => void add()}
          >
            Add webhook
          </Button>
        </>
      }
    >
      <div class="form">
        <label>
          Name
          <input
            value={draft.name}
            maxLength={64}
            placeholder="e.g. Team channel"
            onInput={(e) => {
              const value = e.currentTarget.value;
              setDraft((d) => ({ ...d, name: value }));
            }}
          />
        </label>
        <div class="field">
          Where to
          <div class="segmented" role="radiogroup" aria-label="Where to">
            {FORMATS.map(([id, text]) => (
              <button
                key={id}
                type="button"
                role="radio"
                aria-checked={draft.format === id}
                onClick={() => {
                  setDraft((d) => ({ ...d, format: id }));
                }}
              >
                {text}
              </button>
            ))}
          </div>
        </div>
        <label>
          URL
          <input
            type="password"
            value={draft.url}
            spellcheck={false}
            autoComplete="off"
            placeholder={format[2]}
            onInput={(e) => {
              const value = e.currentTarget.value;
              setDraft((d) => ({ ...d, url: value }));
            }}
          />
          <span class="hint">{format[3]} Kept in the keychain and never shown again.</span>
        </label>
        <div class="field">
          Send
          <div class="chips" role="group" aria-label="Events">
            {EVENTS.map(([id, text]) => (
              <button
                key={id}
                type="button"
                class={`chip${draft.events.includes(id) ? " on" : ""}`}
                aria-pressed={draft.events.includes(id)}
                onClick={() => {
                  setDraft((d) => ({ ...d, events: toggle(d.events, id) }));
                }}
              >
                {text}
              </button>
            ))}
          </div>
        </div>
        {team.length > 1 && (
          <div class="field">
            From
            <div class="chips" role="group" aria-label="Agents">
              <button
                type="button"
                class={`chip${draft.agents.length === 0 ? " on" : ""}`}
                aria-pressed={draft.agents.length === 0}
                onClick={() => {
                  setDraft((d) => ({ ...d, agents: [] }));
                }}
              >
                Every agent
              </button>
              {team.map((a) => (
                <button
                  key={a.id}
                  type="button"
                  class={`chip${draft.agents.includes(a.id) ? " on" : ""}`}
                  aria-pressed={draft.agents.includes(a.id)}
                  onClick={() => {
                    setDraft((d) => ({ ...d, agents: toggle(d.agents, a.id) }));
                  }}
                >
                  {a.name}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>
    </Dialog>
  );
}
