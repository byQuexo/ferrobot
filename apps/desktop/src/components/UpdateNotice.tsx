import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { ArrowUpCircle } from "lucide-preact";
import { check } from "@tauri-apps/plugin-updater";

/** How often the app looks for a newer release while it runs. */
const EVERY_MS = 6 * 60 * 60 * 1000;

/** A note in the sidebar's corner when a newer release is out; it opens Settings → Updates. */
export function UpdateNotice(props: { onOpen: () => void; rail: boolean }): JSX.Element | null {
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    // Only the desktop app has the updater; a dev build or a browser shows nothing.
    if (!window.__FERROBOT_SHELL__) return;
    let live = true;
    const look = (): void => {
      check().then(
        (found) => {
          if (live) setVersion(found?.version ?? null);
        },
        () => undefined,
      );
    };
    look();
    const timer = window.setInterval(look, EVERY_MS);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, []);
  if (!version) return null;
  const label = `Update to ${version}`;
  return (
    <button
      type="button"
      class="update-note"
      title={`${label}: a new release is out`}
      aria-label={label}
      onClick={props.onOpen}
    >
      <ArrowUpCircle size={14} />
      {!props.rail && <span>{label}</span>}
    </button>
  );
}
