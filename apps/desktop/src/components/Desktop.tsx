import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import RFB from "@novnc/novnc";
import { Eye, Hand, Monitor, MonitorOff, Moon, MousePointer2, RotateCw, X } from "lucide-preact";

import { useApp } from "../context.ts";
import { Button, Dialog, IconButton } from "./ui.tsx";

/** A live VNC view of the agent's desktop, scaled to its box; view only unless `control`. */
function Live(props: { agent: string; control: boolean; small?: boolean }): JSX.Element {
  const { client } = useApp();
  const box = useRef<HTMLDivElement>(null);
  const [lost, setLost] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    if (!box.current) return;
    setLost(false);
    const rfb = new RFB(box.current, client.screenUrl(props.agent, props.control), {
      shared: true,
      wsProtocols: ["binary"],
    });
    rfb.viewOnly = !props.control;
    rfb.scaleViewport = true;
    rfb.background = "transparent";
    if (props.control) rfb.focus();
    const onLost = (): void => {
      setLost(true);
    };
    rfb.addEventListener("disconnect", onLost);
    return () => {
      rfb.removeEventListener("disconnect", onLost);
      rfb.disconnect();
    };
  }, [props.agent, props.control, attempt]);
  return (
    <div class="screen-live">
      <div ref={box} class="screen-canvas" />
      {lost && !props.small && (
        <div class="screen-lost">
          <span class="muted small">The connection to the desktop ended.</span>
          <Button
            small
            onClick={() => {
              setAttempt((n) => n + 1);
            }}
          >
            <RotateCw size={16} /> Reconnect
          </Button>
        </div>
      )}
    </div>
  );
}

/** The desktop over the whole window, view only until the operator takes it over. */
function FullScreen(props: { agent: string; name: string; onClose: () => void }): JSX.Element {
  const [control, setControl] = useState(false);
  useEffect(() => {
    // While the operator types into the desktop, Escape belongs to it.
    if (control) return;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") props.onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
    };
  }, [control]);
  return (
    <div
      class="screen-full"
      role="dialog"
      aria-modal="true"
      aria-label={`${props.name}'s computer`}
    >
      <div class="screen-stage">
        <Live agent={props.agent} control={control} />
      </div>
      <span class="screen-title">
        <Monitor size={16} aria-hidden="true" />
        <span class="ellipsis">{props.name}'s computer</span>
      </span>
      <span class="screen-close">
        <IconButton label="Close" onClick={props.onClose}>
          <X size={16} />
        </IconButton>
      </span>
      {control && (
        <p class="screen-note">
          Don't type passwords or keys here: {props.name}'s other tools can still see this screen.
        </p>
      )}
      <div class={`screen-pill${control ? " on" : ""}`}>
        <span class="screen-mode">
          {control ? (
            <MousePointer2 size={16} aria-hidden="true" />
          ) : (
            <Eye size={16} aria-hidden="true" />
          )}
          {control ? `Using it yourself, ${props.name} waits` : "View only"}
        </span>
        {control ? (
          <Button
            small
            onClick={() => {
              setControl(false);
            }}
          >
            <Hand size={16} /> Hand back
          </Button>
        ) : (
          <Button
            small
            variant="primary"
            onClick={() => {
              setControl(true);
            }}
          >
            <MousePointer2 size={16} /> Use it myself
          </Button>
        )}
      </div>
    </div>
  );
}

type Readiness = "checking" | "off" | "building" | "restart" | "ready";

/** The computer card's small screen: live while awake, a placeholder otherwise; opens full. */
export function DesktopPreview(props: {
  agent: string;
  name: string;
  asleep: boolean;
}): JSX.Element | null {
  const { client, run } = useApp();
  const [ready, setReady] = useState<Readiness>("checking");
  const [open, setOpen] = useState(false);
  const [tick, setTick] = useState(0);
  const [asking, setAsking] = useState(false);

  useEffect(() => {
    if (props.asleep) return;
    let live = true;
    const check = async (): Promise<Readiness> => {
      const clis = await client.clis();
      if (!clis.desktop) return "off";
      if (clis.status === "building") return "building";
      const out = await client.exec(props.agent, ["/usr/bin/test", "-x", "/usr/bin/Xvfb"]);
      return out.exit_code === 0 ? "ready" : "restart";
    };
    check().then(
      (r) => {
        if (live) setReady(r);
      },
      () => {
        if (live) setReady("off");
      },
    );
    return () => {
      live = false;
    };
  }, [props.agent, props.asleep, tick]);
  useEffect(() => {
    if (ready !== "building") return;
    const timer = setTimeout(() => {
      setTick((t) => t + 1);
    }, 5000);
    return () => {
      clearTimeout(timer);
    };
  }, [ready, tick]);

  if (props.asleep) {
    return (
      <span class="screen-thumb idle" title="Asleep: the screen shows once the computer is awake">
        <Moon size={16} aria-hidden="true" />
      </span>
    );
  }
  if (ready === "checking") return <span class="screen-thumb idle" />;
  if (ready === "off") {
    return (
      <>
        <button
          type="button"
          class="screen-thumb idle"
          onClick={() => {
            setAsking(true);
          }}
        >
          <MonitorOff size={16} aria-hidden="true" />
          <span class="thumb-note">Set up a desktop</span>
        </button>
        {asking && (
          <Dialog
            title="Set up a desktop?"
            onClose={() => {
              setAsking(false);
            }}
            footer={
              <>
                <Button
                  variant="ghost"
                  onClick={() => {
                    setAsking(false);
                  }}
                >
                  Cancel
                </Button>
                <Button
                  variant="primary"
                  onClick={() =>
                    void run(async () => {
                      await client.updateClis({ desktop: true });
                      setAsking(false);
                      setTick((t) => t + 1);
                    })
                  }
                >
                  Set up
                </Button>
              </>
            }
          >
            <p>
              Every agent's computer gets a screen with a browser, a terminal and a file manager,
              about 170 MB more. ferrobot builds it once; computers pick it up when they next
              restart, and their files stay.
            </p>
            <p class="muted small">
              You can watch and use any agent's screen. An agent uses it on its own only with the
              Desktop tool, which includes a terminal. Turn it off again in Settings → Computers.
            </p>
          </Dialog>
        )}
      </>
    );
  }
  if (ready === "building") {
    return (
      <span class="screen-thumb idle">
        <Monitor size={16} aria-hidden="true" />
        <span class="thumb-note">Setting up the desktop…</span>
      </span>
    );
  }
  if (ready === "restart") {
    return (
      <button
        type="button"
        class="screen-thumb idle"
        title="This computer gets the desktop when it restarts. Restart it now"
        onClick={() =>
          void run(async () => {
            await client.sleep(props.agent);
            await client.wake(props.agent);
            setTick((t) => t + 1);
          })
        }
      >
        <RotateCw size={16} aria-hidden="true" />
        <span class="thumb-note">Restart to get the desktop</span>
      </button>
    );
  }
  return (
    <>
      <button
        type="button"
        class="screen-thumb"
        aria-label={`Open ${props.name}'s screen`}
        title="Open the screen"
        onClick={() => {
          setOpen(true);
        }}
      >
        {!open && <Live agent={props.agent} control={false} small />}
      </button>
      {open && (
        <FullScreen
          agent={props.agent}
          name={props.name}
          onClose={() => {
            setOpen(false);
          }}
        />
      )}
    </>
  );
}
