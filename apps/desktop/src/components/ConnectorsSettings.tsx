import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import {
  BookOpen,
  Bug,
  Check,
  Cloud,
  Code,
  CreditCard,
  Database,
  KeyRound,
  ListChecks,
  ListTodo,
  LogIn,
  LogOut,
  Mail,
  NotebookText,
  Plug,
  Plus,
  RefreshCw,
  Search,
  Settings2,
  Sparkles,
  Trash2,
  Workflow,
} from "lucide-preact";

import type { Connector } from "../api.ts";
import { useApp } from "../context.ts";
import { type CatalogEntry, type CatalogKind, groupCatalog, isAdded } from "../lib/catalog.ts";
import {
  checkDot,
  checkLabel,
  commandLine,
  entryOf,
  monogram,
  needOf,
  ownAppEntry,
  splitCommand,
  tileHue,
  usedByLabel,
  usersOf,
} from "../lib/connectors.ts";
import { Button, Dialog, IconButton } from "./ui.tsx";

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

const KIND_ICONS: Record<CatalogKind, typeof Code> = {
  work: ListTodo,
  notes: NotebookText,
  code: Code,
  errors: Bug,
  cloud: Cloud,
  data: Database,
  money: CreditCard,
  docs: BookOpen,
  search: Search,
  ai: Sparkles,
  mail: Mail,
  automation: Workflow,
};

const AUTH_LABELS: Record<CatalogEntry["auth"], string> = {
  oauth: "Sign in",
  token: "Token",
  none: "No sign-in",
};

/** A catalog server shows its kind's icon, any other its initials; the hue comes from the name. */
/** Which of the server's tools agents with this connector may use; all ticked saves "all". */
function ToolScope(props: {
  connector: Connector;
  busy: boolean;
  onSave: (tools: string[] | null) => void;
}): JSX.Element {
  const names = props.connector.check?.names ?? [];
  const [picked, setPicked] = useState<ReadonlySet<string>>(
    new Set(props.connector.tools ?? names),
  );
  if (names.length === 0)
    return <p class="hint conn-app">Its tools show here once a check reaches the server.</p>;
  const all = names.every((n) => picked.has(n));
  return (
    <form
      class="form conn-app"
      onSubmit={(e) => {
        e.preventDefault();
        props.onSave(all ? null : names.filter((n) => picked.has(n)));
      }}
    >
      <span class="hint">
        Agents with this connector get only the tools ticked here. For a different set per agent,
        add the server again under another name.
      </span>
      <div class="scope-list">
        {names.map((n) => (
          <label key={n} class="check">
            <input
              type="checkbox"
              checked={picked.has(n)}
              onChange={(e) => {
                const next = new Set(picked);
                if (e.currentTarget.checked) next.add(n);
                else next.delete(n);
                setPicked(next);
              }}
            />
            <span class="mono small">{n}</span>
          </label>
        ))}
      </div>
      <div>
        <Button small type="submit" disabled={props.busy}>
          Save tools
        </Button>
      </div>
    </form>
  );
}

function Tile(props: { name: string; kind?: CatalogKind | undefined }): JSX.Element {
  const Icon = props.kind ? KIND_ICONS[props.kind] : undefined;
  return (
    <span
      class="conn-tile"
      style={{ "--tile-hue": String(tileHue(props.name)) }}
      aria-hidden="true"
    >
      {Icon ? <Icon size={18} /> : monogram(props.name)}
    </span>
  );
}

/** Add, sign in and remove connectors; their tokens and client secrets live in Vault. */
export function ConnectorsSettings(props: {
  /** Opens Vault, with the entry of this id (`connector:<id>`, `client:<id>`) ready to fill. */
  onVault: (focus?: string) => void;
  onGuide: () => void;
}): JSX.Element {
  const { client, toast, agents } = useApp();
  const [connectors, setConnectors] = useState<Connector[]>([]);
  const [users, setUsers] = useState<Map<string, string[]>>(new Map());
  const [query, setQuery] = useState("");
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [local, setLocal] = useState(false);
  const [commandText, setCommandText] = useState("");
  const [tokenEnv, setTokenEnv] = useState("");
  const [ownApp, setOwnApp] = useState(false);
  const [app, setApp] = useState<AppDraft>(EMPTY_APP);
  const [editApp, setEditApp] = useState<({ id: string } & AppDraft) | null>(null);
  const [scoping, setScoping] = useState<string | null>(null);
  const [signIn, setSignIn] = useState<{ id: string; url: string } | null>(null);
  const [adding, setAdding] = useState(false);
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState<ReadonlySet<string>>(new Set());
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

  useEffect(() => {
    const assigned = [...agents.values()].map((a) =>
      client.agentConnectors(a.id).then(
        (ids): [string, string[]] => [a.name, ids],
        (): [string, string[]] => [a.name, []],
      ),
    );
    void Promise.all(assigned).then((pairs) => {
      setUsers(usersOf(pairs));
    });
  }, [agents.size, connectors.length]);

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

  const openForm = (draft: { name: string; url: string; app?: AppDraft }): void => {
    setName(draft.name);
    setUrl(draft.url);
    setOwnApp(draft.app !== undefined);
    setApp(draft.app ?? EMPTY_APP);
    setAdding(true);
  };
  const openCustom = (): void => {
    openForm({ name: "", url: "" });
  };

  const addFromCatalog = (e: CatalogEntry): Promise<void> => {
    if (e.app) {
      openForm({
        name: e.name,
        url: e.url,
        app: { clientId: "", issuer: e.app.issuer, scopes: e.app.scopes.join(" ") },
      });
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
        props.onVault(`connector:${c.id}`);
      }
    });
  };

  // A catalog server that needs the operator's own OAuth app cannot be added without one.
  const preset = ownAppEntry(url);
  const lockedApp = preset !== undefined;
  const withApp = ownApp || lockedApp;
  const canAdd =
    !busy &&
    !!name.trim() &&
    (local
      ? splitCommand(commandText).length > 0
      : !!url.trim() && !(withApp && !app.clientId.trim()));

  const add = (): Promise<void> =>
    run(async () => {
      const c = await client.createConnector(
        local
          ? {
              name: name.trim(),
              command: splitCommand(commandText),
              token_env: tokenEnv.trim() || null,
            }
          : {
              name: name.trim(),
              url: url.trim(),
              ...(withApp
                ? {
                    client_id: app.clientId.trim() || null,
                    issuer: app.issuer.trim() || null,
                    scopes: scopesOf(app.scopes),
                  }
                : {}),
            },
      );
      toastIfFailed(c);
      setAdding(false);
    });

  const toggleApp = (c: Connector): void => {
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
  };

  const groups = groupCatalog(query);

  return (
    <div>
      <section class="settings-section">
        <div class="conn-head">
          <h3 class="form-head">Your connectors</h3>
          <span class="spacer" />
          <Button small onClick={openCustom}>
            <Plus size={16} /> Add a custom connector
          </Button>
        </div>
        {connectors.length === 0 ? (
          <p class="muted small">
            No connectors yet. Add one from the catalog below, then give it to an agent in its Edit
            agent dialog.
          </p>
        ) : (
          <ul class="conn-list">
            {connectors.map((c) => {
              const entry = entryOf(c);
              const busyCheck = checking.has(c.id);
              const need = busyCheck ? undefined : needOf(c);
              const tokenInVault = (): void => {
                props.onVault(`connector:${c.id}`);
              };
              return (
                <li key={c.id} class="conn-card">
                  <Tile name={c.name} kind={entry?.kind} />
                  <div class="conn-main">
                    <div class="conn-title">
                      <b>{entry?.name === c.name ? entry.title : c.name}</b>
                      <span class={`conn-status ${checkDot(c, busyCheck)}`}>
                        <span class="dot" aria-hidden="true" />
                        {checkLabel(c, busyCheck)}
                      </span>
                    </div>
                    {c.command ? (
                      <span class="muted small conn-url" title={commandLine(c.command)}>
                        Runs here: {commandLine(c.command)}
                        {c.token_env ? ` · token as ${c.token_env}` : ""}
                      </span>
                    ) : (
                      <span class="muted small conn-url" title={c.url}>
                        {c.url}
                        {c.client_id ? " · own OAuth app" : ""}
                      </span>
                    )}
                    <span class="small conn-users">{usedByLabel(users.get(c.id) ?? [])}</span>
                    {need && (
                      <div class="conn-next">
                        {need === "app" && (
                          <>
                            <Button
                              small
                              variant="primary"
                              onClick={() => {
                                toggleApp(c);
                              }}
                            >
                              <Settings2 size={16} /> Set up your OAuth app
                            </Button>
                            <button type="button" class="text-link small" onClick={props.onGuide}>
                              Guide
                            </button>
                          </>
                        )}
                        {need === "token" && (
                          <Button small variant="primary" onClick={tokenInVault}>
                            <KeyRound size={16} /> Add its token in Vault
                          </Button>
                        )}
                        {need === "sign_in" && (
                          <>
                            <Button
                              small
                              variant="primary"
                              disabled={busy}
                              onClick={() => void startSignIn(c)}
                            >
                              <LogIn size={16} />
                              {c.sign_in === "expired" ? "Sign in again" : "Sign in"}
                              <span class="sr-only"> to {c.name}</span>
                            </Button>
                            <span class="muted small">
                              or add a{" "}
                              <button type="button" class="text-link" onClick={tokenInVault}>
                                token in Vault
                              </button>
                            </span>
                          </>
                        )}
                      </div>
                    )}
                    {c.client_id && !c.has_client_secret && c.sign_in !== "signed_in" && (
                      <span class="hint">
                        No client secret for its OAuth app yet.{" "}
                        <button
                          type="button"
                          class="text-link"
                          onClick={() => {
                            props.onVault(`client:${c.id}`);
                          }}
                        >
                          Add it in Vault
                        </button>
                      </span>
                    )}
                    {signIn?.id === c.id && (
                      <span class="small">
                        Finish signing in in your browser. Nothing opened?{" "}
                        <a href={signIn.url} target="_blank" rel="noreferrer">
                          Open the sign-in page
                        </a>
                        .
                      </span>
                    )}
                  </div>
                  <div class="conn-actions">
                    <IconButton
                      label={`Check ${c.name} again`}
                      disabled={busyCheck}
                      onClick={() => void recheck(c.id)}
                    >
                      <RefreshCw size={16} />
                    </IconButton>
                    <IconButton
                      label={`${c.has_token ? "Replace" : "Add"} the token of ${c.name} in Vault`}
                      onClick={tokenInVault}
                    >
                      <KeyRound size={16} />
                    </IconButton>
                    <IconButton
                      label={`Tools of ${c.name} agents may use`}
                      pressed={scoping === c.id}
                      onClick={() => {
                        setScoping(scoping === c.id ? null : c.id);
                      }}
                    >
                      <ListChecks size={16} />
                    </IconButton>
                    <IconButton
                      label={`OAuth app of ${c.name}`}
                      pressed={editApp?.id === c.id}
                      onClick={() => {
                        toggleApp(c);
                      }}
                    >
                      <Settings2 size={16} />
                    </IconButton>
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
                      label={`Delete ${c.name}`}
                      tone="danger"
                      disabled={busy}
                      onClick={() => void run(() => client.deleteConnector(c.id))}
                    >
                      <Trash2 size={16} />
                    </IconButton>
                  </div>
                  {scoping === c.id && (
                    <ToolScope
                      connector={c}
                      busy={busy}
                      onSave={(tools) =>
                        void run(async () => {
                          await client.updateConnector(c.id, { tools });
                          setScoping(null);
                        })
                      }
                    />
                  )}
                  {editApp?.id === c.id && (
                    <form
                      class="form conn-app"
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
                        Changing the client ID or issuer signs the connector out and removes its
                        client secret from Vault.
                      </span>
                      <div>
                        <Button small type="submit" disabled={busy}>
                          Save OAuth app
                        </Button>
                      </div>
                    </form>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </section>
      <section class="settings-section">
        <div class="conn-head">
          <h3 class="form-head">Catalog</h3>
          <span class="spacer" />
          <label class="search conn-search">
            <Search size={16} aria-hidden="true" />
            <input
              type="search"
              aria-label="Search the catalog"
              placeholder="Search the catalog"
              value={query}
              onInput={(e) => {
                setQuery(e.currentTarget.value);
              }}
            />
          </label>
        </div>
        <p class="muted small">
          Popular MCP servers run by their vendors. Add one, then sign in or add its token.
        </p>
        {groups.map((g) => (
          <div key={g.label} class="conn-group">
            <h4 class="conn-group-head">{g.label}</h4>
            <ul class="catalog" aria-label={g.label}>
              {g.entries.map((e) => (
                <li key={e.name} class="catalog-card">
                  <div class="catalog-head">
                    <Tile name={e.name} kind={e.kind} />
                    <b>{e.title}</b>
                    <span class="spacer" />
                    <span class="conn-auth">{AUTH_LABELS[e.auth]}</span>
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
                    {isAdded(e, connectors) ? (
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
              ))}
            </ul>
          </div>
        ))}
        {groups.length === 0 && (
          <p class="muted small conn-none">Nothing in the catalog matches “{query.trim()}”.</p>
        )}
        <div class="conn-custom">
          <span class="conn-custom-icon" aria-hidden="true">
            <Plug size={18} />
          </span>
          <div class="conn-main">
            <b>Not in the catalog?</b>
            <span class="muted small">Add any remote MCP server by its URL.</span>
          </div>
          <Button small onClick={openCustom}>
            <Plus size={16} /> Add a custom connector
          </Button>
        </div>
      </section>
      {adding && (
        <Dialog
          title={preset ? `Add ${preset.title}` : "Add a custom connector"}
          onClose={() => {
            setAdding(false);
          }}
          footer={
            <>
              <Button
                variant="ghost"
                onClick={() => {
                  setAdding(false);
                }}
              >
                Cancel
              </Button>
              <Button variant="primary" disabled={!canAdd} onClick={() => void add()}>
                Add connector
              </Button>
            </>
          }
        >
          <div class="form">
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
            {!preset && (
              <fieldset class="conn-kind">
                <legend>Where it runs</legend>
                <label class="check">
                  <input
                    type="radio"
                    name="conn-kind"
                    checked={!local}
                    onChange={() => {
                      setLocal(false);
                    }}
                  />
                  A remote server (URL)
                </label>
                <label class="check">
                  <input
                    type="radio"
                    name="conn-kind"
                    checked={local}
                    onChange={() => {
                      setLocal(true);
                    }}
                  />
                  A local server on this computer (command)
                </label>
              </fieldset>
            )}
            {local && (
              <>
                <label>
                  Command
                  <input
                    value={commandText}
                    placeholder="npx -y package-name"
                    onInput={(e) => {
                      setCommandText(e.currentTarget.value);
                    }}
                  />
                  <span class="hint">
                    Runs on this computer with your rights, outside the agents’ sandboxes, when
                    ferrobot starts. Only add servers you trust.
                  </span>
                </label>
                <label>
                  Token variable (optional)
                  <input
                    value={tokenEnv}
                    placeholder="e.g. API_TOKEN"
                    onInput={(e) => {
                      setTokenEnv(e.currentTarget.value);
                    }}
                  />
                  <span class="hint">
                    If the server reads a token from the environment: put the token in Vault after
                    adding it.
                  </span>
                </label>
              </>
            )}
            {!local && (
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
            )}
            {!local && (
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
            )}
            {!local && withApp && (
              <AppFields draft={app} onChange={setApp} redirectUri={redirectUri} />
            )}
            {!local && withApp && !app.clientId.trim() && (
              <span class="hint">
                Enter your OAuth app’s client ID. No app yet?{" "}
                <button type="button" class="text-link" onClick={props.onGuide}>
                  See the guide
                </button>
                .
              </span>
            )}
            <span class="hint">
              After adding it, sign in from its card, or put its token in{" "}
              <button
                type="button"
                class="text-link"
                onClick={() => {
                  props.onVault();
                }}
              >
                Vault
              </button>
              .
            </span>
          </div>
        </Dialog>
      )}
    </div>
  );
}
