import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";

import type { OtelView } from "../api.ts";
import { useApp } from "../context.ts";
import { clockTime } from "../lib/format.ts";
import { authValue, backendOf, OTEL_ENDPOINTS, type OtelBackend } from "../lib/otel.ts";
import { PrefRow } from "./AppearanceSettings.tsx";
import { Button, Switch } from "./ui.tsx";

const BACKENDS: [OtelBackend, string][] = [
  ["phoenix", "Phoenix"],
  ["langfuse", "Langfuse"],
  ["other", "Other"],
];

function statusOf(view: OtelView): string {
  const last = view.last_export;
  if (!last) return view.enabled ? "No turn sent yet." : "";
  const at = clockTime(last.at);
  return last.error
    ? `Last export failed (${at}): ${last.error}`
    : `Last export went through at ${at}.`;
}

/** Trace export to Phoenix, Langfuse or any OTLP/HTTP endpoint; keys go to the daemon's keychain. */
export function TracesSettings(): JSX.Element | null {
  const { client, run } = useApp();
  const [view, setView] = useState<OtelView | null>(null);
  const [backend, setBackend] = useState<OtelBackend>("phoenix");
  const [endpoint, setEndpoint] = useState("");
  const [project, setProject] = useState("");
  const [header, setHeader] = useState("");
  const [key, setKey] = useState("");
  const [secret, setSecret] = useState("");

  const show = (v: OtelView): void => {
    setView(v);
    setBackend(v.endpoint ? backendOf(v.endpoint) : "phoenix");
    setEndpoint(v.endpoint || OTEL_ENDPOINTS.phoenix);
    setProject(v.project ?? "");
    setHeader(v.header ?? "");
    setKey("");
    setSecret("");
  };

  useEffect(() => {
    void run(() => client.otel()).then((v) => {
      if (v) show(v);
    });
  }, []);

  if (!view) return null;
  const choose = (b: OtelBackend): void => {
    setBackend(b);
    if (b !== "other") setEndpoint(OTEL_ENDPOINTS[b]);
    setKey("");
    setSecret("");
  };
  const auth = authValue(backend, key, secret);
  const save = async (enabled: boolean, content = view.content): Promise<void> => {
    const custom = backend === "other";
    // A stored key belongs to the backend it was saved for, never to the next one.
    const switched = view.endpoint !== "" && backendOf(view.endpoint) !== backend;
    const keep = view.has_secret && !switched;
    const headerName = custom ? header.trim() || null : auth || keep ? "Authorization" : null;
    let saved = await run(() =>
      client.updateOtel({
        enabled,
        endpoint,
        header: headerName,
        project: backend === "phoenix" ? project : null,
        content,
      }),
    );
    if (saved && auth) saved = await run(() => client.setOtelSecret(auth));
    else if (saved && switched && view.has_secret)
      saved = await run(() => client.setOtelSecret(null));
    if (saved) show(saved);
  };
  const forget = async (): Promise<void> => {
    const saved = await run(() => client.setOtelSecret(null));
    if (saved) show(saved);
  };
  const input = (
    label: string,
    value: string,
    set: (v: string) => void,
    placeholder: string,
    password = false,
  ): JSX.Element => (
    <input
      aria-label={label}
      type={password ? "password" : "text"}
      value={value}
      spellcheck={false}
      autoComplete="off"
      placeholder={placeholder}
      onInput={(e) => {
        set(e.currentTarget.value);
      }}
    />
  );
  const stored = view.has_secret ? "Stored. Type a new one to replace it." : undefined;

  return (
    <section class="settings-section">
      <ul class="pref-list">
        <PrefRow
          title="Send traces"
          hint="When a turn ends: its model calls and tool calls with times, tokens and errors."
        >
          <Switch checked={view.enabled} label="Send traces" onChange={(v) => void save(v)} />
        </PrefRow>
        <PrefRow
          title="Include content"
          hint="Also send tool inputs (commands, files) and the reply."
        >
          <Switch
            checked={view.content}
            label="Include content"
            onChange={(v) => void save(view.enabled, v)}
          />
        </PrefRow>
        <PrefRow title="Backend">
          <div class="segmented" role="radiogroup" aria-label="Backend">
            {BACKENDS.map(([id, text]) => (
              <button
                key={id}
                type="button"
                role="radio"
                aria-checked={backend === id}
                onClick={() => {
                  choose(id);
                }}
              >
                {text}
              </button>
            ))}
          </div>
        </PrefRow>
        <PrefRow title="Endpoint" hint="OTLP over HTTP. Traces are sent as protobuf.">
          {input("Endpoint", endpoint, setEndpoint, "https://…/v1/traces")}
        </PrefRow>
        {backend === "phoenix" && (
          <>
            <PrefRow title="Project" hint="Optional. Otherwise Phoenix’s default project.">
              {input("Project", project, setProject, "ferrobot")}
            </PrefRow>
            <PrefRow
              title="API key"
              hint={stored ?? "Only for Phoenix Cloud or Phoenix with auth on."}
            >
              {input("API key", key, setKey, "Phoenix API key", true)}
            </PrefRow>
          </>
        )}
        {backend === "langfuse" && (
          <>
            <PrefRow title="Public key" hint={stored ?? "From your Langfuse project’s settings."}>
              {input("Public key", key, setKey, "pk-lf-…")}
            </PrefRow>
            <PrefRow title="Secret key">
              {input("Secret key", secret, setSecret, "sk-lf-…", true)}
            </PrefRow>
          </>
        )}
        {backend === "other" && (
          <>
            <PrefRow title="Header" hint="Optional, e.g. Authorization or x-honeycomb-team.">
              {input("Header", header, setHeader, "Authorization")}
            </PrefRow>
            <PrefRow title="Header value" hint={stored}>
              {input("Header value", key, setKey, "e.g. Bearer …", true)}
            </PrefRow>
          </>
        )}
      </ul>
      <div class="row-actions">
        <Button variant="primary" onClick={() => void save(view.enabled)}>
          Save
        </Button>
        {view.has_secret && (
          <button type="button" class="text-link" onClick={() => void forget()}>
            Remove the stored key
          </button>
        )}
        <span class={`hint${view.last_export?.error ? " bad" : ""}`} role="status">
          {statusOf(view)}
        </span>
      </div>
    </section>
  );
}
