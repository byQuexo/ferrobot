import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { KeyRound, Trash2 } from "lucide-preact";

import type { Connector, LoginView, Provider, VoiceView } from "../api.ts";
import { useApp } from "../context.ts";
import { loginLabel, presetOf } from "../lib/logins.ts";
import { errorText } from "../lib/format.ts";
import { KIND_LABELS, KIND_LINKS, LinkOut, needsKey } from "./ProvidersSettings.tsx";
import { Button, IconButton } from "./ui.tsx";
import { VOICE_LABELS } from "./VoiceSettings.tsx";

interface Entry {
  id: string;
  name: string;
  what: string;
  stored: boolean;
  /** Where to create the key. */
  keyUrl?: string;
  save: (value: string | null) => Promise<unknown>;
}

/** Every stored key in one place. Values go to the daemon’s keychain and never come back. */
export function VaultSettings(props: {
  providersOnly?: boolean;
  /** Called with the providers after every load or change. */
  onProviders?: (providers: Provider[]) => void;
  /** The entry to open for typing, e.g. `connector:<id>`. */
  focus?: string | undefined;
}): JSX.Element {
  const { client, toast } = useApp();
  const [providers, setProviders] = useState<Provider[]>([]);
  const [connectors, setConnectors] = useState<Connector[]>([]);
  const [voice, setVoice] = useState<VoiceView | null>(null);
  const [logins, setLogins] = useState<LoginView[]>([]);
  const [editing, setEditing] = useState<string | null>(props.focus ?? null);
  const focusPending = useRef(props.focus !== undefined);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);

  const run = async (action: () => Promise<unknown>): Promise<void> => {
    setBusy(true);
    try {
      await action();
      const [p, c, v, l] = await Promise.all([
        client.listProviders(),
        client.listConnectors(),
        client.voice(),
        client.logins(),
      ]);
      setProviders(p);
      props.onProviders?.(p);
      setConnectors(c);
      setVoice(v);
      setLogins(l);
    } catch (err) {
      toast(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    void run(() => Promise.resolve());
  }, []);

  const groups: [string, Entry[]][] = [
    [
      "Model providers",
      providers
        .filter((p) => needsKey(p.kind))
        .map((p) => ({
          id: `provider:${p.id}`,
          name: p.name,
          what: `${KIND_LABELS[p.kind]} API key`,
          stored: p.has_key,
          keyUrl: KIND_LINKS[p.kind]?.[0]?.url,
          save: (v) => client.updateProvider(p.id, { api_key: v }),
        })),
    ],
    [
      "Connectors",
      connectors.flatMap((c) => [
        {
          id: `connector:${c.id}`,
          name: c.name,
          what: c.sign_in === "signed_in" ? "Bearer token (signed in with OAuth)" : "Bearer token",
          stored: c.has_token,
          save: (v: string | null) => client.updateConnector(c.id, { token: v }),
        },
        ...(c.client_id
          ? [
              {
                id: `client:${c.id}`,
                name: c.name,
                what: "OAuth client secret",
                stored: c.has_client_secret,
                save: (v: string | null) => client.updateConnector(c.id, { client_secret: v }),
              },
            ]
          : []),
      ]),
    ],
    [
      "CLI logins",
      logins.map((l) => ({
        id: `login:${l.id}`,
        name: loginLabel(l.id),
        what: `Token for ${(l.programs ?? []).join(", ") || l.hosts.join(", ")}`,
        stored: l.has_token,
        keyUrl: presetOf(l.id)?.keyUrl,
        save: (v) => client.setLoginToken(l.id, v),
      })),
    ],
    [
      "Voice",
      (voice?.providers ?? []).map((p) => ({
        id: `voice:${p.kind}`,
        name: VOICE_LABELS[p.kind],
        what: "API key",
        stored: p.has_key,
        save: (v) => client.updateVoiceKey(p.kind, v),
      })),
    ],
  ];

  return (
    <div>
      {(props.providersOnly ? groups.slice(0, 1) : groups).map(([group, entries]) => (
        <section key={group} class="settings-section form">
          <h3 class="form-head">{group}</h3>
          {entries.length === 0 && (
            <p class="muted small">Nothing here yet. Add one in its own section first.</p>
          )}
          <ul class="provider-list">
            {entries.map((e) => (
              <li key={e.id} class="provider-row">
                <span class={`dot ${e.stored ? "p-awake" : "p-sleeping"}`} aria-hidden="true" />
                <div class="provider-main">
                  <b>{e.name}</b>
                  <span class="muted small">
                    {e.what} · {e.stored ? "stored" : "not set"}
                    {!e.stored && e.keyUrl && (
                      <>
                        {" · "}
                        <LinkOut label="create one" url={e.keyUrl} />
                      </>
                    )}
                  </span>
                </div>
                <IconButton
                  label={`${e.stored ? "Replace" : "Add"} ${e.what} of ${e.name}`}
                  onClick={() => {
                    setEditing(editing === e.id ? null : e.id);
                    setValue("");
                  }}
                >
                  <KeyRound size={16} />
                </IconButton>
                {e.stored && (
                  <IconButton
                    label={`Remove ${e.what} of ${e.name}`}
                    tone="danger"
                    disabled={busy}
                    onClick={() => void run(() => e.save(null))}
                  >
                    <Trash2 size={16} />
                  </IconButton>
                )}
                {editing === e.id && (
                  <form
                    class="provider-rekey"
                    onSubmit={(ev) => {
                      ev.preventDefault();
                      void run(async () => {
                        await e.save(value.trim());
                        setEditing(null);
                        setValue("");
                      });
                    }}
                  >
                    <input
                      ref={(el) => {
                        if (!el || !focusPending.current) return;
                        focusPending.current = false;
                        el.focus();
                        el.scrollIntoView({ block: "center" });
                      }}
                      type="password"
                      autoComplete="off"
                      spellcheck={false}
                      aria-label={e.what}
                      placeholder={e.stored ? "New value" : "Value"}
                      value={value}
                      onInput={(ev) => {
                        setValue(ev.currentTarget.value);
                      }}
                    />
                    <Button small type="submit" disabled={busy || !value.trim()}>
                      Save
                    </Button>
                  </form>
                )}
              </li>
            ))}
          </ul>
        </section>
      ))}
    </div>
  );
}
