import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";

import type { UpdateInfo } from "../api.ts";
import { useApp } from "../context.ts";
import { pollOutcome } from "../lib/update.ts";
import { Button } from "./ui.tsx";

const errorText = (err: unknown): string => (err instanceof Error ? err.message : String(err));
const sleep = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(resolve, ms);
  });

/** Reopens the window on the new build; a browser just reloads. */
async function restartApp(): Promise<void> {
  if (window.__FERROBOT_SHELL__) await relaunch();
  else location.reload();
}

/** Release builds: the desktop app's own updater. */
function AppUpdate(): JSX.Element {
  const [update, setUpdate] = useState<Update | null>(null);
  const [status, setStatus] = useState("");
  const [busy, setBusy] = useState(false);

  const checkNow = async (): Promise<void> => {
    setBusy(true);
    setStatus("Checking…");
    try {
      const found = await check();
      setUpdate(found);
      setStatus(found ? `Version ${found.version} is available.` : "You’re up to date.");
    } catch (err) {
      setStatus(`Couldn’t check: ${errorText(err)}`);
    } finally {
      setBusy(false);
    }
  };

  const install = async (found: Update): Promise<void> => {
    setBusy(true);
    let total = 0;
    let done = 0;
    setStatus("Downloading…");
    try {
      await found.downloadAndInstall((event) => {
        if (event.event === "Started") total = event.data.contentLength ?? 0;
        if (event.event === "Progress" && total) {
          done += event.data.chunkLength;
          setStatus(`Downloading… ${Math.round((done * 100) / total)}%`);
        }
        if (event.event === "Finished") setStatus("Installing…");
      });
      await restartApp();
    } catch (err) {
      setStatus(`Update failed: ${errorText(err)}`);
      setBusy(false);
    }
  };

  return (
    <>
      {status && <p>{status}</p>}
      {update?.body && <p class="muted small update-notes">{update.body}</p>}
      {update && !window.__FERROBOT_SHELL__?.ownDaemon && (
        <p class="hint">ferrobotd was started outside the app: restart it too.</p>
      )}
      <div class="row-actions">
        {update ? (
          <Button variant="primary" disabled={busy} onClick={() => void install(update)}>
            Update and restart
          </Button>
        ) : (
          <Button disabled={busy} onClick={() => void checkNow()}>
            Check for updates
          </Button>
        )}
      </div>
    </>
  );
}

/** Source builds: the daemon pulls `main`, rebuilds and restarts itself; then the app relaunches. */
function SourceUpdate(props: {
  info: UpdateInfo;
  onInfo: (info: UpdateInfo) => void;
}): JSX.Element {
  const { client, toast } = useApp();
  const { info } = props;
  const [lost, setLost] = useState(false);

  const follow = async (): Promise<void> => {
    let unreachableSince = 0;
    for (;;) {
      await sleep(1000);
      let next: UpdateInfo | null = null;
      try {
        next = await client.updateInfo();
        unreachableSince = 0;
      } catch {
        unreachableSince ||= Date.now();
        if (Date.now() - unreachableSince > 120_000) {
          setLost(true);
          return;
        }
      }
      if (next) props.onInfo(next);
      const outcome = pollOutcome(next);
      if (outcome === "restarted") {
        await restartApp();
        return;
      }
      if (outcome === "done") return;
    }
  };

  useEffect(() => {
    if (info.state === "running" || info.state === "restarting") void follow();
  }, []);

  const start = async (): Promise<void> => {
    try {
      props.onInfo(await client.startUpdate());
      void follow();
    } catch (err) {
      toast(errorText(err));
    }
  };

  const busy = info.state === "running" || info.state === "restarting";
  return (
    <>
      {info.state === "running" && <p>Updating: {info.step}…</p>}
      {info.state === "restarting" && <p>Restarting…</p>}
      {info.state === "up_to_date" && <p>Already up to date.</p>}
      {info.state === "failed" && (
        <p class="hint bad">
          Update failed{info.step ? ` at ${info.step}` : ""}: {info.error}
        </p>
      )}
      {lost && <p class="hint bad">The daemon didn’t come back. Check its terminal.</p>}
      {(busy || info.state === "failed") && info.log.length > 0 && (
        <pre class="update-log">{info.log.join("\n")}</pre>
      )}
      <div class="row-actions">
        <Button variant="primary" disabled={busy} onClick={() => void start()}>
          Update and restart
        </Button>
        <span class="hint">Pulls main, rebuilds and restarts ferrobot.</span>
      </div>
    </>
  );
}

/** The version ferrobot runs and its update. */
export function UpdatesSettings(): JSX.Element | null {
  const { client, run } = useApp();
  const [info, setInfo] = useState<UpdateInfo | null>(null);

  useEffect(() => {
    const load = async (): Promise<void> => {
      setInfo((await run(() => client.updateInfo())) ?? null);
    };
    void load();
  }, []);

  if (!info) return null;
  return (
    <section class="settings-section form">
      <p>
        ferrobot {info.version}
        {info.commit && <span class="muted"> · {info.commit.slice(0, 7)}</span>}
      </p>
      {info.source_build ? (
        <SourceUpdate info={info} onInfo={setInfo} />
      ) : window.__FERROBOT_SHELL__?.updater ? (
        <AppUpdate />
      ) : (
        <p class="muted">Updates aren’t set up for this build.</p>
      )}
    </section>
  );
}
