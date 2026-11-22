import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import {
  BookOpen,
  Bug,
  Check,
  Cloud,
  Code,
  CreditCard,
  Database,
  ListTodo,
  LogIn,
  LogOut,
  NotebookText,
  Plus,
  Search,
  Settings2,
  Sparkles,
  Trash2,
} from "lucide-preact";

import type { Connector } from "../api.ts";
import { useApp } from "../context.ts";
import { CATALOG, type CatalogEntry, isAdded } from "../lib/catalog.ts";
import { checkDot, checkLabel, ownAppEntry } from "../lib/connectors.ts";
import { Button, IconButton } from "./ui.tsx";

/** Client id, issuer and scopes of an own OAuth app; its client secret lives in Vault. */
interface AppDraft {
  clientId: string;
  issuer: string;
  scopes: string;
}

const EMPTY_APP: AppDraft = { clientId: "", issuer: "", scopes: "" };

function AppFields(props: {
  draft: AppDraft;
  onChange: (d: AppDraft) => void;
  redirectUri: string;
}): JSX.Element {
  const { draft, onChange } = props;
  return (
    <>
      <label>
        OAuth client ID
        <input
          value={draft.clientId}
          autoComplete="off"
          spellcheck={false}
          onInput={(e) => {
            onChange({ ...draft, clientId: e.currentTarget.value });
          }}
        />
      </label>
      <label>
        Issuer
        <input
          value={draft.issuer}
          spellcheck={false}
          placeholder="https://accounts.google.com"
          onInput={(e) => {
            onChange({ ...draft, issuer: e.currentTarget.value });
          }}
        />
        <span class="hint">
          The provider’s sign-in server. Sign-ins through any other server are refused.
        </span>
      </label>
      <label>
        Scopes (optional)
        <input
          value={draft.scopes}
          spellcheck={false}
          placeholder="Space separated"
          onInput={(e) => {
            onChange({ ...draft, scopes: e.currentTarget.value });
          }}
        />
        <span class="hint">
          For providers without automatic registration (Google, GitHub, Slack): create an OAuth app
          there and add this redirect URI: <code>{props.redirectUri}</code>. Its client secret goes
          in Vault.
        </span>
      </label>
    </>
  );
}

const scopesOf = (text: string): string[] => text.split(/\s+/).filter(Boolean);

const KIND_ICONS: Record<CatalogEntry["kind"], typeof Code> = {
  work: ListTodo,
  notes: NotebookText,
  code: Code,
  errors: Bug,
  data: Database,
  cloud: Cloud,
  money: CreditCard,
  docs: BookOpen,
  search: Search,
  ai: Sparkles,
};

const AUTH_LABELS: Record<CatalogEntry["auth"], string> = {
  oauth: "Sign in",
  token: "Token",
  none: "No sign-in",
};

/** Add, sign in and remove connectors; their tokens and client secrets live in Vault. */
export function ConnectorsSettings(props: {
  onVault: () => void;
  onGuide: () => void;
}): JSX.Element {
  const { client, toast } = useApp();
  const [connectors, setConnectors] = useState<Connector[]>([]);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [ownApp, setOwnApp] = useState(false);
  const [app, setApp] = useState<AppDraft>(EMPTY_APP);
  const [editApp, setEditApp] = useState<({ id: string } & AppDraft) | null>(null);
  const [signIn, setSignIn] = useState<{ id: string; url: string } | null>(null);
  const [adding, setAdding] = useState(false);
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState<ReadonlySet<string>>(new Set());
  const form = useRef<HTMLDivElement>(null);
  const redirectUri = connectors[0]?.redirect_uri ?? `${client.base}/v1/oauth/callback`;
  const errorText = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  const run = async (action: () => Promise<unknown>): Promise<void> => {
    setBusy(true);
    try {
      await action();
      setConnectors(await client.listConnectors());
    } catch (err) {
      toast(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  const recheck = async (id: string): Promise<void> => {
    setChecking((ids) => new Set(ids).add(id));
    try {
      const check = await client.checkConnector(id);
      setConnectors((all) => all.map((c) => (c.id === id ? { ...c, check } : c)));
    } catch (err) {
      toast(errorText(err));
    } finally {
      setChecking((ids) => new Set([...ids].filter((i) => i !== id)));
    }
  };

  const toastIfFailed = (c: Connector): void => {
    if (c.check?.status === "failed") toast(`${c.name} ${checkLabel(c, false)}`);
  };

  // A restarted daemon has no checks yet: check those rows without blocking the screen.
  useEffect(() => {
    void (async () => {
      try {
        const all = await client.listConnectors();
        setConnectors(all);
        for (const c of all) if (!c.check) void recheck(c.id);
      } catch (err) {
        toast(errorText(err));
      }
    })();
  }, []);

  // While a browser sign-in is open, refresh until the daemon reports it done.
  useEffect(() => {
    if (!signIn) return;
    let live = true;
    const started = Date.now();
    const poll = (): void => {
      client.listConnectors().then(
        (all) => {
          if (!live) return;
          setConnectors(all);
          if (all.find((c) => c.id === signIn.id)?.sign_in === "signed_in") {
            setSignIn(null);
            void recheck(signIn.id);
          } else if (Date.now() - started < 600_000) setTimeout(poll, 2000);
        },
        () => undefined,
      );
    };
    poll();
    return () => {
      live = false;
    };
  }, [signIn]);

  const startSignIn = (c: Connector): Promise<void> =>
    run(async () => {
      const { authorization_url } = await client.startConnectorOAuth(c.id);
      // The system browser (the app window never shows the provider's page).
      window.open(authorization_url, "_blank", "noopener");
      setSignIn({ id: c.id, url: authorization_url });
    });

  useEffect(() => {
    if (adding) form.current?.scrollIntoView({ behavior: "smooth", block: "center" });
  }, [adding]);

  const addFromCatalog = (e: CatalogEntry): Promise<void> => {
    if (e.app) {
      setName(e.name);
      setUrl(e.url);
      setOwnApp(true);
      setApp({ clientId: "", issuer: e.app.issuer, scopes: e.app.scopes.join(" ") });
      setAdding(true);
      return Promise.resolve();
    }
    return run(async () => {
      const c = await client.createConnector({ name: e.name, url: e.url });
      toastIfFailed(c);
      if (e.auth === "oauth" && c.check?.status === "needs_sign_in") {
        const { authorization_url } = await client.startConnectorOAuth(c.id);
        window.open(authorization_url, "_blank", "noopener");
        setSignIn({ id: c.id, url: authorization_url });
      } else if (e.auth === "token") {
        props.onVault();
      }
    });
  };

  // A catalog server that needs the operator's own OAuth app cannot be added without one.
  const lockedApp = ownAppEntry(url) !== undefined;
  const withApp = ownApp || lockedApp;

  const add = (): Promise<void> =>
    run(async () => {
      const c = await client.createConnector({
        name: name.trim(),
        url: url.trim(),
        ...(withApp
          ? {
              client_id: app.clientId.trim() || null,
              issuer: app.issuer.trim() || null,
              scopes: scopesOf(app.scopes),
            }
          : {}),
      });
      toastIfFailed(c);
      setName("");
      setUrl("");
      setApp(EMPTY_APP);
      setOwnApp(false);
      setAdding(false);
    });

  return (
    <div>
      <section class="settings-section form">
        {connectors.length === 0 && (
          <p class="muted small">
            No connectors yet. A connector is a remote MCP server; agents you give it to can use its
            tools.
          </p>
        )}
        <ul class="provider-list">
          {connectors.map((c) => (
            <li key={c.id} class="provider-row">
              <span class={`dot ${checkDot(c, checking.has(c.id))}`} aria-hidden="true" />
              <div class="provider-main">
                <b>{c.name}</b>
                <span class="muted small">
                  {c.url} · {checkLabel(c, checking.has(c.id))}
                  {c.client_id ? " · own OAuth app" : ""}
                </span>
              </div>
              {c.check?.status === "needs_sign_in" && (
                <IconButton
                  label={`Sign in to ${c.name}`}
                  disabled={busy}
                  onClick={() => void startSignIn(c)}
                >
                  <LogIn size={16} />
                </IconButton>
              )}
              {c.sign_in === "signed_in" && (
                <IconButton
                  label={`Sign out of ${c.name}`}
                  disabled={busy}
                  onClick={() => void run(() => client.signOutConnector(c.id))}
                >
                  <LogOut size={16} />
                </IconButton>
              )}
              <IconButton
                label={`OAuth app of ${c.name}`}
                onClick={() => {
                  setEditApp(
                    editApp?.id === c.id
                      ? null
                      : {
                          id: c.id,
                          clientId: c.client_id ?? "",
                          issuer: c.issuer ?? "",
                          scopes: c.scopes.join(" "),
                        },
                  );
                }}
              >
                <Settings2 size={16} />
              </IconButton>
              <IconButton
                label={`Delete ${c.name}`}
                tone="danger"
                disabled={busy}
                onClick={() => void run(() => client.deleteConnector(c.id))}
              >
                <Trash2 size={16} />
              </IconButton>
              {signIn?.id === c.id && (
                <span class="small provider-login">
                  Finish signing in in your browser. Nothing opened?{" "}
                  <a href={signIn.url} target="_blank" rel="noreferrer">
                    Open the sign-in page
                  </a>
                  .
                </span>
              )}
              {editApp?.id === c.id && (
                <form
                  class="form provider-rekey"
                  onSubmit={(e) => {
                    e.preventDefault();
                    const draft = editApp;
                    void run(async () => {
                      await client.updateConnector(c.id, {
                        client_id: draft.clientId.trim() || null,
                        issuer: draft.issuer.trim() || null,
                        scopes: scopesOf(draft.scopes),
                      });
                      setEditApp(null);
                    });
                  }}
                >
                  <AppFields
                    draft={editApp}
                    onChange={(d) => {
                      setEditApp({ ...d, id: c.id });
                    }}
                    redirectUri={c.redirect_uri}
                  />
                  <span class="hint">
                    Changing the client ID or issuer signs the connector out and removes its client
                    secret from Vault.
                  </span>
                  <div>
                    <Button small type="submit" disabled={busy}>
                      Save OAuth app
                    </Button>
                  </div>
                </form>
              )}
            </li>
          ))}
        </ul>
      </section>
      <section class="settings-section">
        <h3 class="form-head">Catalog</h3>
        <p class="muted small">Popular MCP servers run by their vendors. Add one, then sign in.</p>
        <ul class="catalog">
          {CATALOG.map((e) => {
            const Icon = KIND_ICONS[e.kind];
            const added = isAdded(e, connectors);
            return (
              <li key={e.name} class="catalog-card">
                <div class="catalog-head">
                  <span class="catalog-icon" aria-hidden="true">
                    <Icon size={16} />
                  </span>
                  <b>{e.title}</b>
                  <span class="spacer" />
                  <span class="muted small">{AUTH_LABELS[e.auth]}</span>
                </div>
                <p class="muted small">{e.about}</p>
                {e.setup && (
                  <p class="hint">
                    {e.setup}
                    {e.app && (
                      <>
                        {" "}
                        <button type="button" class="text-link" onClick={props.onGuide}>
                          Guide
                        </button>
                      </>
                    )}
                  </p>
                )}
                <div>
                  {added ? (
                    <span class="muted small catalog-added">
                      <Check size={14} /> Added
                    </span>
                  ) : (
                    <Button small disabled={busy} onClick={() => void addFromCatalog(e)}>
                      <Plus size={16} /> Add <span class="sr-only">{e.title}</span>
                    </Button>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      </section>
      <section class="settings-section">
        {adding ? (
          <div class="form" ref={form}>
            <h3 class="form-head">Add a connector</h3>
            <label>
              Name
              <input
                value={name}
                maxLength={16}
                placeholder="e.g. gmail"
                onInput={(e) => {
                  setName(e.currentTarget.value);
                }}
              />
              <span class="hint">
                Lowercase letters, digits and dashes. Its tools show up as name__tool.
              </span>
            </label>
            <label>
              URL
              <input
                value={url}
                placeholder="https://host/mcp"
                onInput={(e) => {
                  const next = e.currentTarget.value;
                  setUrl(next);
                  const entry = ownAppEntry(next);
                  if (entry?.app && !app.issuer.trim()) {
                    setApp({
                      ...app,
                      issuer: entry.app.issuer,
                      scopes: entry.app.scopes.join(" "),
                    });
                  }
                }}
              />
            </label>
            <label class="check">
              <input
                type="checkbox"
                checked={withApp}
                disabled={lockedApp}
                onChange={(e) => {
                  setOwnApp(e.currentTarget.checked);
                }}
              />
              Use my own OAuth app{lockedApp ? " (this server needs one)" : ""}
            </label>
            {withApp && <AppFields draft={app} onChange={setApp} redirectUri={redirectUri} />}
            {withApp && !app.clientId.trim() && (
              <span class="hint">
                Enter your OAuth app’s client ID. No app yet?{" "}
                <button type="button" class="text-link" onClick={props.onGuide}>
                  See the guide
                </button>
                .
              </span>
            )}
            <span class="hint">
              After adding it, put its token in{" "}
              <button type="button" class="text-link" onClick={props.onVault}>
                Vault
              </button>{" "}
              or sign in with OAuth.
            </span>
            <div class="form-actions">
              <Button
                variant="primary"
                disabled={busy || !name.trim() || !url.trim() || (withApp && !app.clientId.trim())}
                onClick={() => void add()}
              >
                Add connector
              </Button>
              <Button
                variant="ghost"
                onClick={() => {
                  setAdding(false);
                }}
              >
                Cancel
              </Button>
            </div>
          </div>
        ) : (
          <Button
            onClick={() => {
              setAdding(true);
            }}
          >
            <Plus size={16} /> Add connector
          </Button>
        )}
      </section>
    </div>
  );
}
