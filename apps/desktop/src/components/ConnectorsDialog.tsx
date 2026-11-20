import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { KeyRound, LogIn, LogOut, Settings2, Trash2 } from "lucide-preact";

import type { Connector } from "../api.ts";
import { useApp } from "../context.ts";
import { Button, Dialog, IconButton } from "./ui.tsx";

const STATUS: Record<Connector["sign_in"], string> = {
  signed_in: "signed in",
  expired: "sign-in expired, sign in again",
  signed_out: "not signed in",
};

/** Client id, secret and scopes of an own OAuth app (empty secret keeps the stored one). */
interface AppDraft {
  clientId: string;
  clientSecret: string;
  scopes: string;
}

function AppFields(props: { draft: AppDraft; onChange: (d: AppDraft) => void; secretStored: boolean; redirectUri: string }): JSX.Element {
  const { draft, onChange } = props;
  return (
    <>
      <label>
        OAuth client ID
        <input value={draft.clientId} autoComplete="off" spellcheck={false} onInput={(e) => { onChange({ ...draft, clientId: e.currentTarget.value }); }} />
      </label>
      <label>
        Client secret
        <input
          type="password"
          autoComplete="off"
          placeholder={props.secretStored ? "Stored; leave empty to keep it" : ""}
          value={draft.clientSecret}
          onInput={(e) => { onChange({ ...draft, clientSecret: e.currentTarget.value }); }}
        />
      </label>
      <label>
        Scopes (optional)
        <input value={draft.scopes} spellcheck={false} placeholder="Space separated" onInput={(e) => { onChange({ ...draft, scopes: e.currentTarget.value }); }} />
        <span class="hint">
          For providers without automatic registration (Google, GitHub, Slack): create an OAuth app there and add this redirect URI:{" "}
          <code>{props.redirectUri}</code>
        </span>
      </label>
    </>
  );
}

const scopesOf = (text: string): string[] => text.split(/\s+/).filter(Boolean);

/** Add, re-token, sign in and remove connectors. Tokens and secrets go to the daemon’s keychain and never come back. */
export function ConnectorsDialog(props: { onClose: () => void }): JSX.Element {
  const { client, toast } = useApp();
  const [connectors, setConnectors] = useState<Connector[]>([]);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const [ownApp, setOwnApp] = useState(false);
  const [app, setApp] = useState<AppDraft>({ clientId: "", clientSecret: "", scopes: "" });
  const [retoken, setRetoken] = useState<{ id: string; token: string } | null>(null);
  const [editApp, setEditApp] = useState<({ id: string } & AppDraft) | null>(null);
  const [signIn, setSignIn] = useState<{ id: string; url: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const redirectUri = connectors[0]?.redirect_uri ?? `${client.base}/v1/oauth/callback`;

  const run = async (action: () => Promise<unknown>): Promise<void> => {
    setBusy(true);
    try {
      await action();
      setConnectors(await client.listConnectors());
    } catch (err) {
      toast(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => { void run(() => Promise.resolve()); }, []);

  // While a browser sign-in is open, refresh until the daemon reports it done.
  useEffect(() => {
    if (!signIn) return;
    let live = true;
    const started = Date.now();
    const poll = (): void => {
      client.listConnectors().then((all) => {
        if (!live) return;
        setConnectors(all);
        if (all.find((c) => c.id === signIn.id)?.sign_in === "signed_in") setSignIn(null);
        else if (Date.now() - started < 600_000) setTimeout(poll, 2000);
      }, () => undefined);
    };
    poll();
    return () => { live = false; };
  }, [signIn]);

  const startSignIn = (c: Connector): Promise<void> =>
    run(async () => {
      const { authorization_url } = await client.startConnectorOAuth(c.id);
      // The system browser (the app window never shows the provider's page).
      window.open(authorization_url, "_blank", "noopener");
      setSignIn({ id: c.id, url: authorization_url });
    });

  const add = (): Promise<void> =>
    run(async () => {
      await client.createConnector({
        name: name.trim(),
        url: url.trim(),
        token: token.trim() || null,
        ...(ownApp
          ? { client_id: app.clientId.trim() || null, client_secret: app.clientSecret.trim() || null, scopes: scopesOf(app.scopes) }
          : {}),
      });
      setName("");
      setUrl("");
      setToken("");
      setApp({ clientId: "", clientSecret: "", scopes: "" });
      setOwnApp(false);
    });

  return (
    <Dialog title="Connectors" onClose={props.onClose} footer={<Button onClick={props.onClose}>Done</Button>}>
      <div class="form">
        {connectors.length === 0 && (
          <p class="muted small">No connectors yet. A connector is a remote MCP server; agents you give it to can use its tools.</p>
        )}
        <ul class="provider-list">
          {connectors.map((c) => (
            <li key={c.id} class="provider-row">
              <div class="provider-main">
                <b>{c.name}</b>
                <span class="muted small">
                  {c.url}
                  {c.has_token ? " · token stored" : ` · ${STATUS[c.sign_in]}`}
                  {c.client_id ? " · own OAuth app" : ""}
                </span>
              </div>
              {!c.has_token && c.sign_in !== "signed_in" && (
                <IconButton label={`Sign in to ${c.name}`} disabled={busy} onClick={() => void startSignIn(c)}>
                  <LogIn size={15} />
                </IconButton>
              )}
              {c.sign_in === "signed_in" && (
                <IconButton label={`Sign out of ${c.name}`} disabled={busy} onClick={() => void run(() => client.signOutConnector(c.id))}>
                  <LogOut size={15} />
                </IconButton>
              )}
              <IconButton
                label={`OAuth app of ${c.name}`}
                onClick={() => {
                  setEditApp(editApp?.id === c.id ? null : { id: c.id, clientId: c.client_id ?? "", clientSecret: "", scopes: c.scopes.join(" ") });
                }}
              >
                <Settings2 size={15} />
              </IconButton>
              <IconButton label={`Replace the token of ${c.name}`} onClick={() => { setRetoken({ id: c.id, token: "" }); }}>
                <KeyRound size={15} />
              </IconButton>
              <IconButton label={`Delete ${c.name}`} tone="danger" disabled={busy} onClick={() => void run(() => client.deleteConnector(c.id))}>
                <Trash2 size={15} />
              </IconButton>
              {signIn?.id === c.id && (
                <span class="small provider-login">
                  Finish signing in in your browser. Nothing opened?{" "}
                  <a href={signIn.url} target="_blank" rel="noreferrer">Open the sign-in page</a>.
                </span>
              )}
              {editApp?.id === c.id && (
                <form
                  class="form provider-rekey"
                  onSubmit={(e) => {
                    e.preventDefault();
                    const draft = editApp;
                    void run(() =>
                      client.updateConnector(c.id, {
                        client_id: draft.clientId.trim() || null,
                        ...(draft.clientSecret.trim() ? { client_secret: draft.clientSecret.trim() } : {}),
                        scopes: scopesOf(draft.scopes),
                      }),
                    ).then(() => { setEditApp(null); });
                  }}
                >
                  <AppFields draft={editApp} onChange={(d) => { setEditApp({ ...d, id: c.id }); }} secretStored={c.has_client_secret} redirectUri={c.redirect_uri} />
                  <span class="hint">Changing the client ID signs the connector out.</span>
                  <div>
                    <Button small type="submit" disabled={busy}>Save OAuth app</Button>
                  </div>
                </form>
              )}
              {retoken?.id === c.id && (
                <form
                  class="provider-rekey"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void run(() => client.updateConnector(c.id, { token: retoken.token.trim() || null })).then(() => {
                      setRetoken(null);
                    });
                  }}
                >
                  <input
                    type="password"
                    autoComplete="off"
                    placeholder="New token (empty removes it)"
                    value={retoken.token}
                    onInput={(e) => { setRetoken({ id: c.id, token: e.currentTarget.value }); }}
                  />
                  <Button small type="submit" disabled={busy}>Save token</Button>
                </form>
              )}
            </li>
          ))}
        </ul>
        <h3 class="form-head">Add a connector</h3>
        <label>
          Name
          <input value={name} maxLength={16} placeholder="e.g. gmail" onInput={(e) => { setName(e.currentTarget.value); }} />
          <span class="hint">Lowercase letters, digits and dashes. Its tools show up as name__tool.</span>
        </label>
        <label>
          URL
          <input value={url} placeholder="https://host/mcp" onInput={(e) => { setUrl(e.currentTarget.value); }} />
        </label>
        <label>
          Token (optional)
          <input type="password" autoComplete="off" value={token} onInput={(e) => { setToken(e.currentTarget.value); }} />
          <span class="hint">Sent as a Bearer token. Leave it empty to sign in with OAuth after adding. Stored in the keychain of the daemon’s computer; agents never see it.</span>
        </label>
        <label class="check">
          <input type="checkbox" checked={ownApp} onChange={(e) => { setOwnApp(e.currentTarget.checked); }} />
          Use my own OAuth app
        </label>
        {ownApp && <AppFields draft={app} onChange={setApp} secretStored={false} redirectUri={redirectUri} />}
        <div>
          <Button variant="primary" disabled={busy || !name.trim() || !url.trim()} onClick={() => void add()}>
            Add connector
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
