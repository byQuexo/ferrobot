import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { LogIn, Plus, Trash2 } from "lucide-preact";

import type { DeviceCode, LoginStatus, Provider, ProviderKind } from "../api.ts";
import { useApp } from "../context.ts";
import { ModelPicker } from "./ModelPicker.tsx";
import { Button, IconButton } from "./ui.tsx";

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
  gemini_cli: "Gemini CLI",
};

const KIND_HINTS: Partial<Record<ProviderKind, string>> = {
  bedrock: "Uses a Bedrock API key. Model ids look like eu.anthropic.claude-…",
  vertex: "Uses the gcloud login on the daemon’s computer; no key needed.",
  ollama: "Talks to Ollama on the daemon’s computer (127.0.0.1:11434).",
  openai_compatible: "Any server with an OpenAI-style /chat/completions endpoint.",
  codex: "Runs OpenAI’s Codex harness. Add an API key in Vault, or sign in with ChatGPT.",
  gemini_cli: "Runs Google’s Gemini CLI harness (over ACP) with a Gemini API key.",
};

// The daemon's own LOGIN_TIMEOUT in crates/daemon/src/codex.rs.
const LOGIN_TIMEOUT_MS = 15 * 60_000;

/** Device-code sign-in with ChatGPT, done by the daemon host’s Codex. */
function ChatgptLogin(props: { provider: Provider }): JSX.Element {
  const { client, toast } = useApp();
  const [code, setCode] = useState<DeviceCode | null>(null);
  const [status, setStatus] = useState<LoginStatus | null>(null);
  const id = props.provider.id;

  useEffect(() => {
    let live = true;
    const started = Date.now();
    const poll = (): void => {
      client.chatgptLoginStatus(id).then(
        (s) => {
          if (!live) return;
          const expired = s.state === "pending" && Date.now() - started >= LOGIN_TIMEOUT_MS;
          setStatus(expired ? { state: "failed", error: "Sign-in timed out. Sign in again." } : s);
          if (s.state === "pending" && !expired) setTimeout(poll, 2000);
        },
        () => undefined,
      );
    };
    poll();
    return () => {
      live = false;
    };
  }, [code]);

  const start = async (): Promise<void> => {
    try {
      setCode(await client.startChatgptLogin(id));
    } catch (err) {
      toast(err instanceof Error ? err.message : String(err));
    }
  };

  if (status?.state === "signed_in")
    return <span class="muted small provider-login">Signed in with ChatGPT</span>;
  return (
    <div class="provider-login">
      {code && status?.state === "pending" ? (
        <span class="small">
          Open{" "}
          <a href={code.verification_url} target="_blank" rel="noreferrer">
            {code.verification_url}
          </a>{" "}
          and enter <b class="mono">{code.user_code}</b>
        </span>
      ) : (
        <Button small onClick={() => void start()}>
          <LogIn size={16} /> Sign in with ChatGPT
        </Button>
      )}
      {status?.state === "failed" && <span class="hint bad">{status.error}</span>}
    </div>
  );
}

/** Whether a kind takes an API key (kept in Vault). */
export const needsKey = (kind: ProviderKind): boolean => kind !== "vertex" && kind !== "ollama";

/** Fields a kind asks for besides its key. */
function fieldsOf(kind: ProviderKind): { baseUrl: boolean; region: boolean; project: boolean } {
  return {
    baseUrl: kind === "openai_compatible",
    region: kind === "bedrock" || kind === "vertex",
    project: kind === "vertex",
  };
}

const dotOf = (p: Provider): string => (!needsKey(p.kind) || p.has_key ? "p-awake" : "p-sleeping");

/** Add and remove model providers (a settings section); their keys live in Vault. */
export function ProvidersSettings(props: { onVault: () => void }): JSX.Element {
  const { client, toast } = useApp();
  const [providers, setProviders] = useState<Provider[]>([]);
  const [kind, setKind] = useState<ProviderKind>("openai");
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [region, setRegion] = useState("");
  const [project, setProject] = useState("");
  const [model, setModel] = useState("");
  const [adding, setAdding] = useState(false);
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

  useEffect(() => {
    void run(() => Promise.resolve());
  }, []);

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
      });
      setName("");
      setModel("");
      setAdding(false);
    });

  return (
    <div>
      <section class="settings-section form">
        {providers.length === 0 && (
          <p class="muted small">
            No providers yet. Without one, agents use the daemon’s ANTHROPIC_API_KEY.
          </p>
        )}
        <ul class="provider-list">
          {providers.map((p) => (
            <li key={p.id} class="provider-row">
              <span class={`dot ${dotOf(p)}`} aria-hidden="true" />
              <div class="provider-main">
                <b>{p.name}</b>
                <span class="muted small">
                  {KIND_LABELS[p.kind]}
                  {p.default_model ? ` · ${p.default_model}` : ""}
                  {!needsKey(p.kind) ? " · no key needed" : p.has_key ? " · key in Vault" : " · "}
                  {needsKey(p.kind) && !p.has_key && (
                    <button type="button" class="text-link" onClick={props.onVault}>
                      add its key in Vault
                    </button>
                  )}
                </span>
              </div>
              <IconButton
                label={`Delete ${p.name}`}
                tone="danger"
                disabled={busy}
                onClick={() => void run(() => client.deleteProvider(p.id))}
              >
                <Trash2 size={16} />
              </IconButton>
              {p.kind === "codex" && !p.has_key && <ChatgptLogin provider={p} />}
            </li>
          ))}
        </ul>
      </section>
      <section class="settings-section">
        {adding ? (
          <div class="form">
            <h3 class="form-head">Add a provider</h3>
            <label>
              Kind
              <select
                value={kind}
                onChange={(e) => {
                  setKind(e.currentTarget.value as ProviderKind);
                }}
              >
                {(Object.keys(KIND_LABELS) as ProviderKind[]).map((k) => (
                  <option key={k} value={k}>
                    {KIND_LABELS[k]}
                  </option>
                ))}
              </select>
              {KIND_HINTS[kind] && <span class="hint">{KIND_HINTS[kind]}</span>}
            </label>
            <label>
              Name
              <input
                value={name}
                maxLength={64}
                placeholder="e.g. openai-work"
                onInput={(e) => {
                  setName(e.currentTarget.value);
                }}
              />
            </label>
            {fields.baseUrl && (
              <label>
                Base URL
                <input
                  value={baseUrl}
                  placeholder="https://host/v1"
                  onInput={(e) => {
                    setBaseUrl(e.currentTarget.value);
                  }}
                />
              </label>
            )}
            {fields.region && (
              <label>
                Region
                <input
                  value={region}
                  placeholder={kind === "vertex" ? "e.g. us-east5 or global" : "e.g. eu-central-1"}
                  onInput={(e) => {
                    setRegion(e.currentTarget.value);
                  }}
                />
              </label>
            )}
            {fields.project && (
              <label>
                Google Cloud project
                <input
                  value={project}
                  onInput={(e) => {
                    setProject(e.currentTarget.value);
                  }}
                />
              </label>
            )}
            <label>
              Default model
              <ModelPicker kind={kind} value={model} placeholder="None" onChange={setModel} />
            </label>
            <div class="form-actions">
              <Button variant="primary" disabled={busy || !name.trim()} onClick={() => void add()}>
                Add provider
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
            <Plus size={16} /> Add provider
          </Button>
        )}
      </section>
    </div>
  );
}
