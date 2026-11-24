import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Copy, Trash2 } from "lucide-preact";

import type {
  IncomingWebhookCreated,
  OutgoingWebhookView,
  WebhookEvent,
  WebhookFormat,
  WebhooksView,
} from "../api.ts";
import { useApp } from "../context.ts";
import { PrefRow } from "./AppearanceSettings.tsx";
import { Button, IconButton } from "./ui.tsx";

const EVENTS: [WebhookEvent, string][] = [
  ["reply", "Replies in your chats"],
  ["question", "Cards waiting for you"],
  ["task_done", "Finished tasks"],
  ["turn_failed", "Failed turns"],
];

const FORMATS: [WebhookFormat, string][] = [
  ["discord", "Discord"],
  ["slack", "Slack"],
  ["json", "JSON"],
];

const label = <T extends string>(options: [T, string][], id: T): string =>
  options.find(([o]) => o === id)?.[1] ?? id;

function delivery(hook: OutgoingWebhookView): string {
  const last = hook.last_delivery;
  if (!last) return "Nothing sent yet.";
  const at = new Date(last.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return last.error
    ? `Last delivery failed (${at}): ${last.error}`
    : `Last delivery went through at ${at}.`;
}

interface Draft {
  name: string;
  url: string;
  format: WebhookFormat;
  events: WebhookEvent[];
  agents: string[];
}

const EMPTY: Draft = { name: "", url: "", format: "discord", events: ["reply"], agents: [] };

/** Incoming webhooks (a POST becomes a message) and outgoing ones (events go to a URL). */
export function WebhooksSettings(props: { onGuide: () => void }): JSX.Element | null {
  const { client, run, agents, toast } = useApp();
  const [view, setView] = useState<WebhooksView | null>(null);
  const [shown, setShown] = useState<IncomingWebhookCreated | null>(null);
  const [name, setName] = useState("");
  const [agent, setAgent] = useState("");
  const [draft, setDraft] = useState<Draft>(EMPTY);

  const load = async (): Promise<void> => {
    const loaded = await run(() => client.webhooks());
    if (loaded) setView(loaded);
  };
  useEffect(() => {
    void load();
  }, []);

  if (!view) return null;
  const team = [...agents.values()];
  const target = agent || team[0]?.id || "";
  const agentName = (id: string): string => agents.get(id)?.name ?? "a deleted agent";
  const copy = async (text: string): Promise<void> => {
    const copied = await run(async () => {
      await navigator.clipboard.writeText(text);
      return true;
    });
    if (copied) toast("Copied.", "info");
  };
  const addIncoming = async (): Promise<void> => {
    const created = await run(() =>
      client.createIncomingWebhook({ name: name.trim(), agent_id: target }),
    );
    if (!created) return;
    setShown(created);
    setName("");
    await load();
  };
  const newUrl = async (id: number): Promise<void> => {
    const created = await run(() => client.regenerateIncomingWebhook(id));
    if (created) setShown(created);
  };
  const removeIncoming = async (id: number): Promise<void> => {
    await run(() => client.deleteIncomingWebhook(id));
    if (shown?.id === id) setShown(null);
    await load();
  };
  const addOutgoing = async (): Promise<void> => {
    const added = await run(() =>
      client.createOutgoingWebhook({ ...draft, name: draft.name.trim(), url: draft.url.trim() }),
    );
    if (!added) return;
    setDraft(EMPTY);
    await load();
  };
  const removeOutgoing = async (id: number): Promise<void> => {
    await run(() => client.deleteOutgoingWebhook(id));
    await load();
  };
  const toggle = <T,>(list: T[], item: T, on: boolean): T[] =>
    on ? [...list, item] : list.filter((x) => x !== item);
  const outgoingAbout = (h: OutgoingWebhookView): string =>
    [
      label(FORMATS, h.format),
      h.events.map((e) => label(EVENTS, e)).join(", "),
      h.agents.length ? h.agents.map(agentName).join(", ") : "every agent",
    ].join(" · ");

  return (
    <section class="settings-section">
      <h3 class="pref-head">Incoming</h3>
      <p class="pref-sub">
        A POST to a webhook’s URL becomes a message to its agent, marked as outside data, not
        instructions. The URL works on this computer; for GitHub and other services on the internet,
        expose only <code>/hooks/</code> through a tunnel.{" "}
        <button type="button" class="text-link" onClick={props.onGuide}>
          How
        </button>
      </p>
      <ul class="pref-list">
        {view.incoming.map((h) => (
          <PrefRow key={h.id} title={h.name} hint={`To ${agentName(h.agent_id)}`}>
            <Button small onClick={() => void newUrl(h.id)}>
              New URL
            </Button>
            <IconButton
              label={`Delete webhook ${h.name}`}
              tone="danger"
              onClick={() => void removeIncoming(h.id)}
            >
              <Trash2 size={16} />
            </IconButton>
          </PrefRow>
        ))}
        <li class="pref-row hook-add">
          <input
            aria-label="Name"
            value={name}
            maxLength={64}
            spellcheck={false}
            autoComplete="off"
            placeholder="Name, e.g. github"
            onInput={(e) => {
              setName(e.currentTarget.value);
            }}
          />
          <select
            aria-label="Agent"
            value={target}
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
          <Button small disabled={!name.trim() || !target} onClick={() => void addIncoming()}>
            Add
          </Button>
        </li>
      </ul>
      {shown && (
        <div class="webhook-new" role="status">
          <span class="hint">
            The URL of <strong>{shown.name}</strong>, shown only now. Anyone with it can message{" "}
            {agentName(shown.agent_id)}.
          </span>
          <div class="webhook-url">
            <code>{shown.url}</code>
            <Button small onClick={() => void copy(shown.url)}>
              <Copy size={14} /> Copy
            </Button>
          </div>
          <span class="hint">Try it:</span>
          <pre class="code-block">
            <code>{`curl -X POST -H 'Content-Type: application/json' \\\n  -d '{"text": "Hello from curl"}' \\\n  '${shown.url}'`}</code>
          </pre>
          <button
            type="button"
            class="text-link"
            onClick={() => {
              setShown(null);
            }}
          >
            Done
          </button>
        </div>
      )}

      <h3 class="pref-head">Outgoing</h3>
      <p class="pref-sub">
        Events you pick go to a Discord channel, a Slack channel or any URL that takes JSON.
      </p>
      {view.outgoing.length > 0 && (
        <ul class="pref-list">
          {view.outgoing.map((h) => (
            <PrefRow
              key={h.id}
              title={h.name}
              hint={
                <>
                  {outgoingAbout(h)}
                  <br />
                  {delivery(h)}
                </>
              }
            >
              <IconButton
                label={`Delete webhook ${h.name}`}
                tone="danger"
                onClick={() => void removeOutgoing(h.id)}
              >
                <Trash2 size={16} />
              </IconButton>
            </PrefRow>
          ))}
        </ul>
      )}
      <div class="form webhook-add">
        <label>
          Name
          <input
            value={draft.name}
            maxLength={64}
            placeholder="e.g. Team channel"
            onInput={(e) => {
              setDraft({ ...draft, name: e.currentTarget.value });
            }}
          />
        </label>
        <label>
          URL
          <input
            type="password"
            value={draft.url}
            spellcheck={false}
            autoComplete="off"
            placeholder="https://discord.com/api/webhooks/…"
            onInput={(e) => {
              setDraft({ ...draft, url: e.currentTarget.value });
            }}
          />
          <span class="hint">
            Kept in the keychain of the daemon’s computer and never shown again.
          </span>
        </label>
        <div class="field">
          Format
          <div class="segmented" role="radiogroup" aria-label="Format">
            {FORMATS.map(([id, text]) => (
              <button
                key={id}
                type="button"
                role="radio"
                aria-checked={draft.format === id}
                onClick={() => {
                  setDraft({ ...draft, format: id });
                }}
              >
                {text}
              </button>
            ))}
          </div>
        </div>
        <fieldset class="group-members">
          <legend>Events</legend>
          {EVENTS.map(([id, text]) => (
            <label key={id} class="check">
              <input
                type="checkbox"
                checked={draft.events.includes(id)}
                onChange={(e) => {
                  setDraft({ ...draft, events: toggle(draft.events, id, e.currentTarget.checked) });
                }}
              />
              {text}
            </label>
          ))}
        </fieldset>
        {team.length > 1 && (
          <fieldset class="group-members">
            <legend>Agents (none checked: every agent)</legend>
            {team.map((a) => (
              <label key={a.id} class="check">
                <input
                  type="checkbox"
                  checked={draft.agents.includes(a.id)}
                  onChange={(e) => {
                    setDraft({
                      ...draft,
                      agents: toggle(draft.agents, a.id, e.currentTarget.checked),
                    });
                  }}
                />
                {a.name}
              </label>
            ))}
          </fieldset>
        )}
        <div class="form-actions">
          <Button
            variant="primary"
            disabled={!draft.name.trim() || !draft.url.trim() || draft.events.length === 0}
            onClick={() => void addOutgoing()}
          >
            Add
          </Button>
        </div>
      </div>
    </section>
  );
}
