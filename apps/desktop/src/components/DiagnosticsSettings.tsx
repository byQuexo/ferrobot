import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";

import type { Diagnostics } from "../api.ts";
import { useApp } from "../context.ts";
import { diagnosticsReport, diagnosticsRows } from "../lib/diagnostics.ts";
import { Button } from "./ui.tsx";

/** apps/desktop/package.json's version, set by vite.config.ts. */
declare const __APP_VERSION__: string;

const LOG_LINES = 200;

/** Versions, status and the daemon log, for bug reports. */
export function DiagnosticsSettings(): JSX.Element | null {
  const { client, run, toast } = useApp();
  const [info, setInfo] = useState<Diagnostics | null>(null);
  const [log, setLog] = useState<string[]>([]);
  const logBox = useRef<HTMLPreElement>(null);

  const load = async (): Promise<[Diagnostics, string[]] | undefined> => {
    const loaded = await run(() =>
      Promise.all([client.diagnostics(), client.diagnosticsLog(LOG_LINES)]),
    );
    if (!loaded) return undefined;
    setInfo(loaded[0]);
    setLog(loaded[1].lines);
    return [loaded[0], loaded[1].lines];
  };

  useEffect(() => {
    void load();
  }, []);

  useEffect(() => {
    if (logBox.current) logBox.current.scrollTop = logBox.current.scrollHeight;
  }, [log]);

  const copy = async (text: string, done: string): Promise<void> => {
    const copied = await run(async () => {
      await navigator.clipboard.writeText(text);
      return true;
    });
    if (copied) toast(done, "info");
  };

  const copyReport = async (): Promise<void> => {
    const loaded = await load();
    if (loaded) await copy(diagnosticsReport(__APP_VERSION__, ...loaded), "Diagnostics copied.");
  };

  if (!info) return null;
  return (
    <section class="settings-section form">
      <div class="kv">
        {diagnosticsRows(__APP_VERSION__, info).map(([label, value]) => [
          <span key={`${label}-label`}>{label}</span>,
          <span key={label}>{value}</span>,
        ])}
        <span>Data folder</span>
        <span class="model-line">
          <span class="mono">{info.data_dir}</span>
          <Button small onClick={() => void copy(info.data_dir, "Path copied.")}>
            Copy path
          </Button>
        </span>
      </div>
      <span class="field">
        <span class="field-row">
          Daemon log
          <Button small onClick={() => void load()}>
            Refresh
          </Button>
        </span>
        <pre class="update-log" ref={logBox}>
          {log.length ? log.join("\n") : "No log lines yet."}
        </pre>
      </span>
      <div class="row-actions">
        <Button variant="primary" onClick={() => void copyReport()}>
          Copy diagnostics
        </Button>
        <span class="hint">For bug reports. Keys and tokens are redacted.</span>
      </div>
    </section>
  );
}
