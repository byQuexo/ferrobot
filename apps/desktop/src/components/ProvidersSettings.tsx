import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { LogIn, Plus, Trash2 } from "lucide-preact";

import type { ClaudeCodeStatus, DeviceCode, LoginStatus, Provider, ProviderKind } from "../api.ts";
import { useApp } from "../context.ts";
import { CLAUDE_CODE_NOTICE, claudeCodeState } from "../lib/claude-code.ts";
import { errorText } from "../lib/format.ts";
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
  claude_code: "Claude Code (your own login)",
};

const KIND_HINTS: Partial<Record<ProviderKind, string>> = {
  bedrock: "Uses a Bedrock API key. Model ids look like eu.anthropic.claude-…",
  vertex: "Uses the gcloud login on the daemon’s computer; no key needed.",
  ollama: "Talks to Ollama on the daemon’s computer (127.0.0.1:11434).",
  openai_compatible: "Any server with an OpenAI-style /chat/completions endpoint.",
  codex: "Runs OpenAI’s Codex harness. Add an API key in Vault, or sign in with ChatGPT.",
  gemini_cli: "Runs Google’s Gemini CLI harness (over ACP) with a Gemini API key.",
  claude_code: `Runs your own claude CLI on the daemon’s computer, off the model proxy. ${CLAUDE_CODE_NOTICE}`,
};

const CODEX_INSTALL: Link = {
  label: "Install Codex to sign in with ChatGPT",
  url: "https://developers.openai.com/codex/cli",
};

interface Link {
  label: string;
  url: string;
}

/** Where to create a kind's key or set it up; a kind's first link is its key page, if it has one. */
export const KIND_LINKS: Partial<Record<ProviderKind, Link[]>> = {
  anthropic: [
    { label: "Create an Anthropic API key", url: "https://platform.claude.com/settings/keys" },
  ],
  openai: [{ label: "Create an OpenAI API key", url: "https://platform.openai.com/api-keys" }],
  gemini: [{ label: "Create a Gemini API key", url: "https://aistudio.google.com/apikey" }],
  bedrock: [
    {
      label: "Create a Bedrock API key",
      url: "https://docs.aws.amazon.com/bedrock/latest/userguide/api-keys.html",
    },
  ],
  vertex: [
    {
      label: "Set up Claude on Vertex AI",
      url: "https://cloud.google.com/vertex-ai/generative-ai/docs/partner-models/claude",
    },
  ],
  openrouter: [
    { label: "Create an OpenRouter API key", url: "https://openrouter.ai/settings/keys" },
  ],
  ollama: [{ label: "Download Ollama", url: "https://ollama.com/download" }],
  codex: [
    { label: "Create an OpenAI API key", url: "https://platform.openai.com/api-keys" },
    CODEX_INSTALL,
  ],
  gemini_cli: [{ label: "Create a Gemini API key", url: "https://aistudio.google.com/apikey" }],
  claude_code: [
    {
      label: "How to log in to Claude Code",
      url: "https://code.claude.com/docs/en/authentication",
    },
    { label: "Anthropic’s terms", url: "https://code.claude.com/docs/en/legal-and-compliance" },
  ],
};

export const LinkOut = (props: Link): JSX.Element => (
  <a href={props.url} target="_blank" rel="noreferrer">
    {props.label}
  </a>
);

// The daemon's own LOGIN_TIMEOUT in crates/daemon/src/codex.rs.
const LOGIN_TIMEOUT_MS = 15 * 60_000;

/** Device-code sign-in with ChatGPT, done by the daemon host’s Codex. */
export function ChatgptLogin(props: {
  provider: Provider;
  /** Offers an OpenAI key in Vault instead; left out where the key row is already shown. */
  onVault?: () => void;
  onStatus?: (status: LoginStatus) => void;
}): JSX.Element {
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
          const shown: LoginStatus = expired
            ? { state: "failed", error: "Sign-in timed out. Sign in again." }
            : s;
          setStatus(shown);
          props.onStatus?.(shown);
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
      toast(errorText(err));
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
      {status?.state === "failed" && !code && (
        // Checking the sign-in failed, mostly because Codex is missing on the daemon's computer.
        <span class="small">
          <LinkOut {...CODEX_INSTALL} />
          {props.onVault && (
            <>
              {" · or "}
              <button type="button" class="text-link" onClick={props.onVault}>
                use an OpenAI API key in Vault
              </button>
            </>
          )}
        </span>
      )}
    </div>
  );
}

/** Whether a kind takes an API key (kept in Vault). */
export const needsKey = (kind: ProviderKind): boolean =>
  kind !== "vertex" && kind !== "ollama" && kind !== "claude_code";

/** Whether the daemon host’s own claude CLI is installed and logged in; ferrobot never logs it in. */
export function ClaudeCodeLogin(props: {
  onStatus?: (status: ClaudeCodeStatus) => void;
}): JSX.Element {
  const { client } = useApp();
  const [status, setStatus] = useState<ClaudeCodeStatus | null>(null);
  const [check, setCheck] = useState(0);

  useEffect(() => {
    let live = true;
    setStatus(null);
    client.claudeCode().then(
      (s) => {
        if (!live) return;
        setStatus(s);
        props.onStatus?.(s);
      },
      () => undefined,
    );
    return () => {
      live = false;
    };
  }, [check]);

  const state = claudeCodeState(status);
  return (
    <div class="provider-login">
      <span class={status && !state.ready ? "hint bad" : "muted small"}>{state.text}</span>
      {status && !state.ready && (
        <span class="small">
          {!status.installed && (
            <>
              <LinkOut label="Install Claude Code" url="https://code.claude.com/docs/en/setup" />
              {" · "}
            </>
          )}
          <button
            type="button"
            class="text-link"
            onClick={() => {
              setCheck(check + 1);
            }}
          >
            Check again
          </button>
        </span>
      )}
    </div>
  );
}

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
export function ProvidersSettings(props: {
  onVault: () => void;
  /** The ChatGPT sign-in on Codex providers without a key; the welcome shows its own. */
  signIn?: boolean;
  /** Called with the list after every load or change. */
  onChange?: (providers: Provider[]) => void;
}): JSX.Element {
  const { client, toast, health } = useApp();
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
      const list = await client.listProviders();
      setProviders(list);
      props.onChange?.(list);
    } catch (err) {
      toast(errorText(err));
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
            {health?.driver === "claude" && !health.anthropic_key
              ? "No providers yet. Agents need one to reach a model."
              : "No providers yet. Without one, agents use the daemon’s ANTHROPIC_API_KEY."}
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
                  {p.kind === "claude_code"
                    ? " · your own login"
                    : !needsKey(p.kind)
                      ? " · no key needed"
                      : p.has_key
                        ? " · key in Vault"
                        : " · "}
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
              {props.signIn !== false && p.kind === "claude_code" && <ClaudeCodeLogin />}
              {props.signIn !== false && p.kind === "codex" && !p.has_key && (
                <ChatgptLogin provider={p} onVault={props.onVault} />
              )}
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
              {KIND_LINKS[kind] && (
                <span class="hint">
                  {KIND_LINKS[kind].map((link, i) => (
                    <span key={link.url}>
                      {i > 0 && " · "}
                      <LinkOut {...link} />
                    </span>
                  ))}
                </span>
              )}
            </label>
            {kind === "claude_code" && <ClaudeCodeLogin />}
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
