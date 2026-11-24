import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import {
  ExternalLink,
  LayoutDashboard,
  Maximize,
  Pencil,
  RefreshCw,
  Trash2,
  X,
} from "lucide-preact";

import { useApp } from "../context.ts";
import { clockTime } from "../lib/format.ts";
import { boardLabel, isBoard, toggleBoard } from "../lib/dashboards.ts";
import { IconButton } from "./ui.tsx";

/** The sidebar's dashboards: one click opens each. */
export function DashboardList(props: { active: number | null }): JSX.Element | null {
  const { dashboards, openDashboard, agents } = useApp();
  if (dashboards.length === 0) return null;
  return (
    <section class="convo-section" aria-label="Dashboards">
      <h2 class="section-label">Dashboards</h2>
      {dashboards.map((d, i) => (
        <button
          key={`${d.agent_id}:${d.path}`}
          type="button"
          class={`convo-row board-row${props.active === i ? " active" : ""}`}
          onClick={() => {
            openDashboard(i);
          }}
        >
          <LayoutDashboard size={18} aria-hidden="true" />
          <span class="convo-text">
            <span class="agent-name">{d.label}</span>
            <span class="agent-desc">{agents.get(d.agent_id)?.name ?? ""}</span>
          </span>
        </button>
      ))}
    </section>
  );
}

/** On a shared HTML file: keep it as a dashboard, or stop. */
export function DashboardButton(props: { agent: string; path: string; name: string }): JSX.Element {
  const { dashboards, saveDashboards } = useApp();
  const on = isBoard(dashboards, props.agent, props.path);
  const label = on ? "Remove from dashboards" : "Use as dashboard";
  return (
    <button
      type="button"
      class={`pin-btn${on ? " on" : ""}`}
      aria-label={label}
      title={label}
      aria-pressed={on}
      onClick={() =>
        void saveDashboards(
          toggleBoard(dashboards, {
            agent_id: props.agent,
            path: props.path,
            label: boardLabel(props.name),
          }),
        )
      }
    >
      <LayoutDashboard size={14} />
    </button>
  );
}

/** A dashboard in the main area, with tabs to switch; it shows the newest shared version. */
export function DashboardView(props: {
  index: number;
  onIndex: (index: number) => void;
  onClose: () => void;
}): JSX.Element | null {
  const { client, dashboards, saveDashboards, reloadDashboards, agents, toast } = useApp();
  const board = dashboards[props.index];
  const [src, setSrc] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const [renaming, setRenaming] = useState(false);
  const frame = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setSrc(null);
    if (!board?.file) return;
    let live = true;
    client.sharedFile(board.agent_id, board.file).then(
      (f) => {
        if (live && f.page_url) setSrc(`${client.base}${f.page_url}`);
      },
      (err: unknown) => {
        toast(err instanceof Error ? err.message : String(err));
      },
    );
    return () => {
      live = false;
    };
  }, [board?.agent_id, board?.file, reload]);

  if (!board) return null;
  const owner = agents.get(board.agent_id)?.name ?? "an agent";
  const rename = (value: string): void => {
    setRenaming(false);
    const label = value.trim();
    if (!label || label === board.label) return;
    void saveDashboards(dashboards.map((d, i) => (i === props.index ? { ...d, label } : d)));
  };

  return (
    <main class="dashboard" aria-label={`Dashboard ${board.label}`}>
      <header class="dashboard-head">
        <div class="dashboard-tabs" role="tablist" aria-label="Dashboards">
          {dashboards.map((d, i) =>
            i === props.index && renaming ? (
              <input
                key="rename"
                class="dashboard-rename"
                aria-label="Dashboard name"
                defaultValue={d.label}
                maxLength={60}
                ref={(el) => el?.select()}
                onKeyDown={(e) => {
                  if (e.key === "Enter") rename(e.currentTarget.value);
                  if (e.key === "Escape") {
                    e.stopPropagation();
                    setRenaming(false);
                  }
                }}
                onBlur={(e) => {
                  rename(e.currentTarget.value);
                }}
              />
            ) : (
              <button
                key={`${d.agent_id}:${d.path}`}
                type="button"
                role="tab"
                aria-selected={i === props.index}
                class={`dashboard-tab${i === props.index ? " active" : ""}`}
                onClick={() => {
                  props.onIndex(i);
                }}
                onDblClick={() => {
                  if (i === props.index) setRenaming(true);
                }}
              >
                {d.label}
              </button>
            ),
          )}
        </div>
        <span class="spacer" />
        <IconButton
          label="Rename"
          onClick={() => {
            setRenaming(true);
          }}
        >
          <Pencil size={16} />
        </IconButton>
        <IconButton
          label="Reload"
          onClick={() => {
            reloadDashboards();
            setReload((n) => n + 1);
          }}
        >
          <RefreshCw size={16} />
        </IconButton>
        <IconButton
          label="Open in your browser"
          disabled={!src}
          onClick={() => {
            if (src) window.open(src, "_blank", "noopener");
          }}
        >
          <ExternalLink size={16} />
        </IconButton>
        <IconButton
          label="Full screen"
          disabled={!src}
          onClick={() => {
            void frame.current?.requestFullscreen().catch(() => {
              if (src) window.open(src, "_blank", "noopener");
            });
          }}
        >
          <Maximize size={16} />
        </IconButton>
        <IconButton
          label={`Remove ${board.label} from dashboards`}
          tone="danger"
          onClick={() => void saveDashboards(dashboards.filter((_, i) => i !== props.index))}
        >
          <Trash2 size={16} />
        </IconButton>
        <IconButton label="Close" onClick={props.onClose}>
          <X size={16} />
        </IconButton>
      </header>
      <p class="dashboard-meta muted small">
        {board.updated_at
          ? `${board.path.split("/").pop() ?? board.path} from ${owner}, shared ${clockTime(board.updated_at)}. It updates when ${owner} shares it again.`
          : `${owner} has not shared ${board.path} yet.`}
      </p>
      <div class="dashboard-frame" ref={frame}>
        {src ? (
          <iframe key={src} title={board.label} sandbox="allow-scripts" src={src} />
        ) : (
          board.file === null && (
            <p class="muted pad">
              The dashboard shows here once {owner} shares <code>{board.path}</code>.
            </p>
          )
        )}
      </div>
    </main>
  );
}
