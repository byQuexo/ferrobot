import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";

import type { ClisView } from "../api.ts";
import { useApp } from "../context.ts";
import { Button } from "./ui.tsx";

/** Keyless tools that install cleanly on the runner image (Debian bookworm). */
const CATALOG: [string, string][] = [
  ["jq", "JSON"],
  ["sqlite3", "SQLite databases"],
  ["poppler-utils", "PDF text and pages (pdftotext)"],
  ["pandoc", "Document conversion"],
  ["imagemagick", "Images"],
  ["ffmpeg", "Audio and video"],
  ["graphviz", "Diagrams (dot)"],
  ["zip", "Zip archives"],
  ["unzip", "Unpack zip archives"],
  ["tree", "Directory trees"],
  ["python3-pip", "Python with pip"],
  ["python3-venv", "Python virtual environments"],
];

/** Debian policy; the daemon checks again. */
const PACKAGE = /^[a-z0-9][a-z0-9+.-]{1,62}$/;

const STATUS: Record<ClisView["status"], string> = {
  idle: "Agents use the plain image.",
  building: "Building the image. This can take a few minutes.",
  ready: "Ready. Agents get these tools when their computer next starts or wakes.",
  failed: "Build failed.",
};

/** One shared image for every agent's computer: the runner image plus Debian packages. */
export function ClisSettings(): JSX.Element | null {
  const { client, run } = useApp();
  const [view, setView] = useState<ClisView | null>(null);
  const [draft, setDraft] = useState<string[]>([]);
  const [other, setOther] = useState("");

  useEffect(() => {
    const load = async (): Promise<void> => {
      const v = await run(() => client.clis());
      if (v) {
        setView(v);
        setDraft(v.packages);
      }
    };
    void load();
  }, []);

  const building = view?.status === "building";
  useEffect(() => {
    if (!building) return;
    const timer = setInterval(() => {
      client.clis().then(setView, () => undefined);
    }, 2000);
    return () => {
      clearInterval(timer);
    };
  }, [building]);

  if (!view) return null;
  if (!view.supported) {
    return (
      <section class="settings-section form">
        <p class="muted small">CLIs need the Docker backend.</p>
      </section>
    );
  }

  const toggle = (pkg: string): void => {
    setDraft(draft.includes(pkg) ? draft.filter((p) => p !== pkg) : [...draft, pkg].sort());
  };
  const addOther = (): void => {
    const pkg = other.trim();
    if (!PACKAGE.test(pkg)) return;
    if (!draft.includes(pkg)) toggle(pkg);
    setOther("");
  };
  const apply = async (): Promise<void> => {
    const saved = await run(() => client.updateClis(draft));
    if (saved) setView(saved);
  };
  const extras = draft.filter((p) => !CATALOG.some(([pkg]) => pkg === p));
  const unchanged = draft.join(" ") === view.packages.join(" ");

  return (
    <section class="settings-section form">
      <span class="field">
        Tools
        <div class="chips" role="group" aria-label="CLIs">
          {[...CATALOG, ...extras.map((p): [string, string] => [p, "Remove"])].map(
            ([pkg, about]) => (
              <button
                key={pkg}
                type="button"
                class={`chip${draft.includes(pkg) ? " on" : ""}`}
                aria-pressed={draft.includes(pkg)}
                title={about}
                onClick={() => {
                  toggle(pkg);
                }}
              >
                {pkg}
              </button>
            ),
          )}
        </div>
        <span class="hint">
          Tools that need a login (gh, aws, gcloud) are left out: keys never enter a sandbox. Use a
          connector for those.
        </span>
      </span>
      <span class="field">
        Other Debian package
        <span class="choice-custom">
          <input
            value={other}
            maxLength={63}
            spellcheck={false}
            autoComplete="off"
            placeholder="e.g. htop"
            onInput={(e) => {
              setOther(e.currentTarget.value);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") addOther();
            }}
          />
          <Button small onClick={addOther} disabled={!PACKAGE.test(other.trim())}>
            Add
          </Button>
        </span>
      </span>
      <div class="row-actions">
        <Button
          variant="primary"
          disabled={building || (unchanged && view.status !== "failed")}
          onClick={() => void apply()}
        >
          Build image
        </Button>
        <span class={`hint${view.status === "failed" ? " bad" : ""}`} role="status">
          {STATUS[view.status]} {view.error ?? ""}
        </span>
      </div>
    </section>
  );
}
