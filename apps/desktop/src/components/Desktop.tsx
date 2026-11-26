import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import RFB from "@novnc/novnc";
import { Hand, Monitor, MonitorOff, Moon, MousePointer2, RotateCw, X } from "lucide-preact";

import { useApp } from "../context.ts";
import { Button, IconButton } from "./ui.tsx";

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
      <header class="screen-head">
        <Monitor size={16} aria-hidden="true" />
        <strong class="ellipsis">{props.name}'s computer</strong>
        <span class={`screen-mode${control ? " on" : ""}`}>
          {control ? `Using it yourself, ${props.name} waits` : "View only"}
        </span>
        <span class="grow" />
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
        <IconButton label="Close" onClick={props.onClose}>
          <X size={16} />
        </IconButton>
      </header>
      <div class="screen-stage">
        <Live agent={props.agent} control={control} />
      </div>
    </div>
  );
}

type Readiness = "checking" | "off" | "restart" | "ready";

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

  useEffect(() => {
    if (props.asleep) return;
    let live = true;
    const check = async (): Promise<Readiness> => {
      if (!(await client.clis()).desktop) return "off";
      const out = await client.exec(props.agent, ["sh", "-c", "command -v Xvfb"]);
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
      <span class="screen-thumb idle" title="No desktop: turn it on in Settings → CLIs">
        <MonitorOff size={16} aria-hidden="true" />
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
