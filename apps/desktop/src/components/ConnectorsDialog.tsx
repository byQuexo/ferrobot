import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { KeyRound, Trash2 } from "lucide-preact";

import type { Connector } from "../api.ts";
import { useApp } from "../context.ts";
import { Button, Dialog, IconButton } from "./ui.tsx";

/** Add, re-token and remove connectors. Tokens go to the daemon’s keychain and never come back. */
export function ConnectorsDialog(props: { onClose: () => void }): JSX.Element {
  const { client, toast } = useApp();
  const [connectors, setConnectors] = useState<Connector[]>([]);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const [retoken, setRetoken] = useState<{ id: string; token: string } | null>(null);
  const [busy, setBusy] = useState(false);

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

  const add = (): Promise<void> =>
    run(async () => {
      await client.createConnector({ name: name.trim(), url: url.trim(), token: token.trim() || null });
      setName("");
      setUrl("");
      setToken("");
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
                  {c.has_token ? " · token stored" : " · no token"}
                </span>
              </div>
              <IconButton label={`Replace the token of ${c.name}`} onClick={() => { setRetoken({ id: c.id, token: "" }); }}>
                <KeyRound size={15} />
              </IconButton>
              <IconButton label={`Delete ${c.name}`} tone="danger" disabled={busy} onClick={() => void run(() => client.deleteConnector(c.id))}>
                <Trash2 size={15} />
              </IconButton>
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
          <input value={name} maxLength={16} placeholder="e.g. github" onInput={(e) => { setName(e.currentTarget.value); }} />
          <span class="hint">Lowercase letters, digits and dashes. Its tools show up as name__tool.</span>
        </label>
        <label>
          URL
          <input value={url} placeholder="https://host/mcp" onInput={(e) => { setUrl(e.currentTarget.value); }} />
        </label>
        <label>
          Token (optional)
          <input type="password" autoComplete="off" value={token} onInput={(e) => { setToken(e.currentTarget.value); }} />
          <span class="hint">Sent as a Bearer token. Stored in the keychain of the daemon’s computer; agents never see it.</span>
        </label>
        <div>
          <Button variant="primary" disabled={busy || !name.trim() || !url.trim()} onClick={() => void add()}>
            Add connector
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
