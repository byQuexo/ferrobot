import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { KeyRound, LogIn, Trash2 } from "lucide-preact";

import type { DeviceCode, LoginStatus, Provider, ProviderKind } from "../api.ts";
import { useApp } from "../context.ts";
import { Button, Dialog, IconButton } from "./ui.tsx";

export const KIND_LABELS: Record<ProviderKind, string> = {
  anthropic: "Anthropic",
  openai: "OpenAI",
  gemini: "Google Gemini",
  bedrock: "Amazon Bedrock (Claude)",
  vertex: "Google Vertex AI (Claude)",
  openrouter: "OpenRouter",
  ollama: "Ollama (local)",
  openai_compatible: "OpenAI-compatible",
  codex: "OpenAI Codex",
};

const KIND_HINTS: Partial<Record<ProviderKind, string>> = {
  bedrock: "Uses a Bedrock API key. Model ids look like eu.anthropic.claude-…",
  vertex: "Uses the gcloud login on the daemon’s computer; no key needed.",
  ollama: "Talks to Ollama on the daemon’s computer (127.0.0.1:11434).",
  openai_compatible: "Any server with an OpenAI-style /chat/completions endpoint.",
  codex: "Runs OpenAI’s Codex harness. Add an API key, or leave it empty and sign in with ChatGPT.",
};

const optionalKey = (kind: ProviderKind): boolean => kind === "openai_compatible" || kind === "codex";

/** Device-code sign-in with ChatGPT, done by the daemon host’s Codex. */
function ChatgptLogin(props: { provider: Provider }): JSX.Element {
  const { client, toast } = useApp();
  const [code, setCode] = useState<DeviceCode | null>(null);
  const [status, setStatus] = useState<LoginStatus | null>(null);
  const id = props.provider.id;

  useEffect(() => {
    let live = true;
    const poll = (): void => {
      client.chatgptLoginStatus(id).then((s) => {
        if (!live) return;
        setStatus(s);
        if (s.state === "pending") setTimeout(poll, 2000);
      }, () => undefined);
    };
    poll();
    return () => { live = false; };
  }, [code]);

  const start = async (): Promise<void> => {
    try {
      setCode(await client.startChatgptLogin(id));
    } catch (err) {
      toast(err instanceof Error ? err.message : String(err));
    }
  };

  if (status?.state === "signed_in") return <span class="muted small provider-login">Signed in with ChatGPT</span>;
  return (
    <div class="provider-login">
      {code && status?.state === "pending" ? (
        <span class="small">
          Open <a href={code.verification_url} target="_blank" rel="noreferrer">{code.verification_url}</a> and enter{" "}
          <b class="mono">{code.user_code}</b>
        </span>
      ) : (
        <Button small onClick={() => void start()}>
          <LogIn size={14} /> Sign in with ChatGPT
        </Button>
      )}
      {status?.state === "failed" && <span class="hint bad">{status.error}</span>}
    </div>
  );
}

/** Fields a kind asks for. */
function fieldsOf(kind: ProviderKind): { key: boolean; baseUrl: boolean; region: boolean; project: boolean } {
  return {
    key: kind !== "vertex" && kind !== "ollama",
    baseUrl: kind === "openai_compatible",
    region: kind === "bedrock" || kind === "vertex",
    project: kind === "vertex",
  };
}

/** Add, re-key and remove model providers. Keys go to the daemon’s keychain and never come back. */
export function ProvidersDialog(props: { onClose: () => void }): JSX.Element {
  const { client, toast } = useApp();
  const [providers, setProviders] = useState<Provider[]>([]);
  const [kind, setKind] = useState<ProviderKind>("openai");
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [region, setRegion] = useState("");
  const [project, setProject] = useState("");
  const [model, setModel] = useState("");
  const [key, setKey] = useState("");
  const [rekey, setRekey] = useState<{ id: string; key: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const fields = fieldsOf(kind);

  const run = async (action: () => Promise<unknown>): Promise<void> => {
    setBusy(true);
    try {
      await action();
      setProviders(await client.listProviders());
    } catch (err) {
      toast(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => { void run(() => Promise.resolve()); }, []);

  const add = (): Promise<void> =>
    run(async () => {
      const opt = (v: string, on: boolean): string | null => (on && v.trim() ? v.trim() : null);
      await client.createProvider({
        name: name.trim(),
        kind,
        base_url: opt(baseUrl, fields.baseUrl),
        region: opt(region, fields.region),
        project: opt(project, fields.project),
        default_model: opt(model, true),
        api_key: opt(key, fields.key),
      });
      setName("");
      setKey("");
      setModel("");
    });

  return (
    <Dialog title="Model providers" onClose={props.onClose} footer={<Button onClick={props.onClose}>Done</Button>}>
      <div class="form">
        {providers.length === 0 && (
          <p class="muted small">No providers yet. Without one, agents use the daemon’s ANTHROPIC_API_KEY.</p>
        )}
        <ul class="provider-list">
          {providers.map((p) => (
            <li key={p.id} class="provider-row">
              <div class="provider-main">
                <b>{p.name}</b>
                <span class="muted small">
                  {KIND_LABELS[p.kind]}
                  {p.default_model ? ` · ${p.default_model}` : ""}
                  {fieldsOf(p.kind).key ? (p.has_key ? " · key stored" : " · no key") : ""}
                </span>
              </div>
              {fieldsOf(p.kind).key && (
                <IconButton label={`Replace the key of ${p.name}`} onClick={() => { setRekey({ id: p.id, key: "" }); }}>
                  <KeyRound size={15} />
                </IconButton>
              )}
              <IconButton
                label={`Delete ${p.name}`}
                tone="danger"
                disabled={busy}
                onClick={() => void run(() => client.deleteProvider(p.id))}
              >
                <Trash2 size={15} />
              </IconButton>
              {p.kind === "codex" && !p.has_key && <ChatgptLogin provider={p} />}
              {rekey?.id === p.id && (
                <form
                  class="provider-rekey"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void run(() => client.updateProvider(p.id, { api_key: rekey.key.trim() || null })).then(() => {
                      setRekey(null);
                    });
                  }}
                >
                  <input
                    type="password"
                    autoComplete="off"
                    placeholder="New API key (empty removes it)"
                    value={rekey.key}
                    onInput={(e) => { setRekey({ id: p.id, key: e.currentTarget.value }); }}
                  />
                  <Button small type="submit" disabled={busy}>Save key</Button>
                </form>
              )}
            </li>
          ))}
        </ul>
        <h3 class="form-head">Add a provider</h3>
        <label>
          Kind
          <select value={kind} onChange={(e) => { setKind(e.currentTarget.value as ProviderKind); }}>
            {(Object.keys(KIND_LABELS) as ProviderKind[]).map((k) => (
              <option key={k} value={k}>{KIND_LABELS[k]}</option>
            ))}
          </select>
          {KIND_HINTS[kind] && <span class="hint">{KIND_HINTS[kind]}</span>}
        </label>
        <label>
          Name
          <input value={name} maxLength={64} placeholder="e.g. openai-work" onInput={(e) => { setName(e.currentTarget.value); }} />
        </label>
        {fields.baseUrl && (
          <label>
            Base URL
            <input value={baseUrl} placeholder="https://host/v1" onInput={(e) => { setBaseUrl(e.currentTarget.value); }} />
          </label>
        )}
        {fields.region && (
          <label>
            Region
            <input
              value={region}
              placeholder={kind === "vertex" ? "e.g. us-east5 or global" : "e.g. eu-central-1"}
              onInput={(e) => { setRegion(e.currentTarget.value); }}
            />
          </label>
        )}
        {fields.project && (
          <label>
            Google Cloud project
            <input value={project} onInput={(e) => { setProject(e.currentTarget.value); }} />
          </label>
        )}
        <label>
          Default model
          <input value={model} placeholder="Used when an agent does not pick one" onInput={(e) => { setModel(e.currentTarget.value); }} />
        </label>
        {fields.key && (
          <label>
            API key{optionalKey(kind) ? " (optional)" : ""}
            <input type="password" autoComplete="off" value={key} onInput={(e) => { setKey(e.currentTarget.value); }} />
            <span class="hint">Stored in the keychain of the daemon’s computer. Agents never see it.</span>
          </label>
        )}
        <div>
          <Button variant="primary" disabled={busy || !name.trim()} onClick={() => void add()}>
            Add provider
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
