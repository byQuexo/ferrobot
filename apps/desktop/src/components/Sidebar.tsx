import type { JSX } from "preact";
import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import {
  ArrowDown,
  ArrowUp,
  ChartColumn,
  PanelLeftClose,
  PanelLeftOpen,
  Pencil,
  Pin,
  Plus,
  Search,
  Settings,
  Star,
  Trash2,
} from "lucide-preact";

import type { Agent } from "../api.ts";
import { useApp } from "../context.ts";
import { rowLine, statusOf } from "../lib/format.ts";
import { loadOrder, moveId, orderAgents, saveOrder } from "../lib/order.ts";
import { Avatar } from "./Avatar.tsx";
import { TagChips } from "./AgentMeta.tsx";
import { ConversationList, type Pair } from "./Conversations.tsx";
import { GroupList } from "./Groups.tsx";
import { RoomList } from "./Rooms.tsx";
import { UpdateNotice } from "./UpdateNotice.tsx";
import { Button, Dialog, IconButton, Logo } from "./ui.tsx";

/** Drag-and-drop and the right-click menu, shared by both kinds of agent entries. */
interface RowHandlers {
  draggable: boolean;
  class: string;
  onDragStart: (e: DragEvent) => void;
  onDragOver: (e: DragEvent) => void;
  onDrop: (e: DragEvent) => void;
  onDragEnd: () => void;
  onContextMenu: (e: MouseEvent) => void;
}

export function Sidebar(props: {
  /** Collapsed to a narrow rail of avatars. */
  rail: boolean;
  onRail: (rail: boolean) => void;
  selected: string | null;
  onSelect: (id: string) => void;
  pair: Pair | null;
  onOpenPair: (p: Pair) => void;
  room: number | null;
  onOpenRoom: (id: number) => void;
  onCreate: () => void;
  onSettings: () => void;
  /** Opens Settings → Updates. */
  onUpdates: () => void;
  settingsOpen: boolean;
  onUsage: () => void;
  usageOpen: boolean;
  online: boolean;
  onDeleted: (id: string) => void;
}): JSX.Element {
  const { agents, client, run } = useApp();
  const [filter, setFilter] = useState("");
  const [order, setOrder] = useState<string[]>(loadOrder);
  const [drop, setDrop] = useState<{ id: string; after: boolean } | null>(null);
  const [menu, setMenu] = useState<{ agent: Agent; x: number; y: number } | null>(null);
  const [renaming, setRenaming] = useState<{ agent: Agent; name: string } | null>(null);
  const rename = async (): Promise<void> => {
    if (!renaming) return;
    const name = renaming.name.trim();
    if (!name || name === renaming.agent.name) return;
    const saved = await run(() => client.updateAgent(renaming.agent.id, { name }));
    if (saved) setRenaming(null);
  };
  const [deleting, setDeleting] = useState<Agent | null>(null);
  const dragged = useRef<string | null>(null);
  const aside = useRef<HTMLElement>(null);
  const toggled = useRef(false);
  const toggle = (): void => {
    toggled.current = true;
    props.onRail(!props.rail);
  };
  // The pressed toggle unmounts, so focus moves to its counterpart.
  useEffect(() => {
    if (!toggled.current) return;
    toggled.current = false;
    aside.current?.querySelector<HTMLElement>("[aria-expanded]")?.focus();
  }, [props.rail]);
  const chatOpen = !props.pair && props.room === null;
  const list = useMemo(() => {
    const f = filter.trim().toLowerCase();
    const shown = [...agents.values()].filter(
      (a) => !f || a.name.includes(f) || a.description.toLowerCase().includes(f),
    );
    return orderAgents(shown, order);
  }, [agents, filter, order]);

  const reorder = (id: string, target: string, after: boolean): void => {
    const next = moveId(
      orderAgents([...agents.values()], order).map((a) => a.id),
      id,
      target,
      after,
    );
    setOrder(next);
    saveOrder(next);
  };
  /** Moves `a` one place up or down within its group (pinned or not). */
  const step = (a: Agent, dir: -1 | 1): void => {
    const group = orderAgents([...agents.values()], order).filter(
      (x) => Boolean(x.pinned) === Boolean(a.pinned),
    );
    const neighbour = group[group.findIndex((x) => x.id === a.id) + dir];
    if (neighbour) reorder(a.id, neighbour.id, dir > 0);
  };
  const handlers = (a: Agent, horizontal: boolean): RowHandlers => ({
    draggable: true,
    class: drop?.id === a.id ? (drop.after ? " drop-after" : " drop-before") : "",
    onDragStart: (e) => {
      dragged.current = a.id;
      e.dataTransfer?.setData("text/plain", a.id);
      if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
    },
    onDragOver: (e) => {
      const from = dragged.current && agents.get(dragged.current);
      // Pinned and other agents keep their own places.
      if (!from || from.id === a.id || Boolean(from.pinned) !== Boolean(a.pinned)) return;
      e.preventDefault();
      const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
      const after = horizontal
        ? e.clientX > r.left + r.width / 2
        : e.clientY > r.top + r.height / 2;
      if (drop?.id !== a.id || drop.after !== after) setDrop({ id: a.id, after });
    },
    onDrop: (e) => {
      e.preventDefault();
      if (dragged.current && drop) reorder(dragged.current, drop.id, drop.after);
      setDrop(null);
      dragged.current = null;
    },
    onDragEnd: () => {
      setDrop(null);
      dragged.current = null;
    },
    onContextMenu: (e) => {
      e.preventDefault();
      setMenu({ agent: a, x: e.clientX, y: e.clientY });
    },
  });

  const railAgents = props.rail ? orderAgents([...agents.values()], order) : [];
  const railItem = (a: Agent): JSX.Element => (
    <RailAgent
      key={a.id}
      agent={a}
      active={chatOpen && a.id === props.selected}
      onSelect={props.onSelect}
      onContextMenu={handlers(a, false).onContextMenu}
    />
  );
  const connLabel = props.online ? "Daemon connected" : "Daemon offline";

  return (
    <aside class={`sidebar${props.rail ? " rail" : ""}`} ref={aside}>
      {props.rail ? (
        <>
          <nav class="rail-list" aria-label="Agents">
            {railAgents.filter((a) => a.pinned).map(railItem)}
            {railAgents.some((a) => a.pinned) && railAgents.some((a) => !a.pinned) && (
              <hr class="rail-divider" />
            )}
            {railAgents.filter((a) => !a.pinned).map(railItem)}
          </nav>
          <div class="rail-tools">
            <IconButton label="Usage" onClick={props.onUsage} pressed={props.usageOpen}>
              <ChartColumn size={18} />
            </IconButton>
            <IconButton label="Settings" onClick={props.onSettings} pressed={props.settingsOpen}>
              <Settings size={18} />
            </IconButton>
            <IconButton label="New agent" onClick={props.onCreate} tone="accent">
              <Plus size={18} />
            </IconButton>
            <IconButton label="Expand sidebar" expanded={false} onClick={toggle}>
              <PanelLeftOpen size={18} />
            </IconButton>
          </div>
        </>
      ) : (
        <>
          <header class="brand">
            <Logo />
            <span class="brand-name">ferrobot</span>
            <span class="spacer" />
            <IconButton label="Usage" onClick={props.onUsage} pressed={props.usageOpen}>
              <ChartColumn size={18} />
            </IconButton>
            <IconButton label="Settings" onClick={props.onSettings} pressed={props.settingsOpen}>
              <Settings size={18} />
            </IconButton>
            <IconButton label="New agent" onClick={props.onCreate} tone="accent">
              <Plus size={18} />
            </IconButton>
            <IconButton label="Collapse sidebar" expanded onClick={toggle}>
              <PanelLeftClose size={18} />
            </IconButton>
          </header>
          <label class="search">
            <Search size={16} aria-hidden="true" />
            <input
              type="search"
              placeholder="Find an agent"
              value={filter}
              onInput={(e) => {
                setFilter(e.currentTarget.value);
              }}
            />
          </label>
          <nav class="agent-list" aria-label="Agents">
            {list.some((a) => a.pinned) && (
              <div class="pinned-strip" aria-label="Pinned agents">
                {list
                  .filter((a) => a.pinned)
                  .map((a) => (
                    <PinnedAgent
                      key={a.id}
                      agent={a}
                      active={chatOpen && a.id === props.selected}
                      onSelect={props.onSelect}
                      row={handlers(a, true)}
                    />
                  ))}
              </div>
            )}
            {list
              .filter((a) => !a.pinned)
              .map((a) => (
                <AgentRow
                  key={a.id}
                  agent={a}
                  active={chatOpen && a.id === props.selected}
                  onSelect={props.onSelect}
                  row={handlers(a, false)}
                />
              ))}
            {list.length === 0 && agents.size > 0 && (
              <p class="muted pad">No agent matches “{filter}”.</p>
            )}
            <GroupList active={props.room} onOpen={props.onOpenRoom} />
            <ConversationList active={props.pair} onOpen={props.onOpenPair} />
            <RoomList active={props.room} onOpen={props.onOpenRoom} />
          </nav>
        </>
      )}
      {menu && (
        <AgentMenu
          {...menu}
          onClose={() => {
            setMenu(null);
          }}
          onStep={(dir) => {
            step(menu.agent, dir);
          }}
          onDelete={() => {
            setDeleting(menu.agent);
          }}
          onRename={() => {
            setRenaming({ agent: menu.agent, name: menu.agent.name });
          }}
        />
      )}
      {renaming && (
        <Dialog
          title={`Rename ${renaming.agent.name}`}
          onClose={() => {
            setRenaming(null);
          }}
          footer={
            <>
              <Button
                onClick={() => {
                  setRenaming(null);
                }}
              >
                Cancel
              </Button>
              <Button
                variant="primary"
                type="submit"
                disabled={!renaming.name.trim() || renaming.name.trim() === renaming.agent.name}
                onClick={() => void rename()}
              >
                Rename
              </Button>
            </>
          }
        >
          <form
            class="form"
            onSubmit={(e) => {
              e.preventDefault();
              void rename();
            }}
          >
            <label>
              Name
              <input
                value={renaming.name}
                maxLength={32}
                autoFocus
                spellcheck={false}
                onInput={(e) => {
                  setRenaming({ ...renaming, name: e.currentTarget.value.toLowerCase() });
                }}
              />
              <span class="hint">
                Lowercase letters, digits and dashes. Its computer and chat stay the same.
              </span>
            </label>
          </form>
        </Dialog>
      )}
      {deleting && (
        <Dialog
          title={`Delete ${deleting.name}?`}
          onClose={() => {
            setDeleting(null);
          }}
          footer={
            <>
              <Button
                onClick={() => {
                  setDeleting(null);
                }}
              >
                Cancel
              </Button>
              <Button
                variant="danger"
                onClick={() =>
                  void run(async () => {
                    await client.deleteAgent(deleting.id);
                    props.onDeleted(deleting.id);
                    setDeleting(null);
                  })
                }
              >
                Delete for good
              </Button>
            </>
          }
        >
          <p>
            This removes {deleting.name}, its computer, chat, memory and routines. It cannot be
            undone.
          </p>
        </Dialog>
      )}
      <footer class="sidebar-foot">
        <span
          class={`conn ${props.online ? "on" : "off"}`}
          title={props.rail ? connLabel : undefined}
        >
          <span class="conn-dot" />
          <span class={props.rail ? "sr-only" : undefined}>{connLabel}</span>
        </span>
        <UpdateNotice onOpen={props.onUpdates} rail={props.rail} />
      </footer>
    </aside>
  );
}

function PinToggle(props: { agent: Agent }): JSX.Element {
  const { client, run } = useApp();
  const a = props.agent;
  return (
    <button
      type="button"
      class="pin-toggle"
      aria-pressed={Boolean(a.pinned)}
      aria-label={a.pinned ? `Unpin ${a.name}` : `Pin ${a.name}`}
      title={a.pinned ? "Unpin" : "Pin to the top"}
      onClick={() => void run(() => client.updateAgent(a.id, { pinned: !a.pinned }))}
    >
      <Pin size={16} />
    </button>
  );
}

/** Right-click menu of an agent; the move items are the keyboard way to reorder. */
function AgentMenu(props: {
  agent: Agent;
  x: number;
  y: number;
  onClose: () => void;
  onStep: (dir: -1 | 1) => void;
  onDelete: () => void;
  onRename: () => void;
}): JSX.Element {
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    box.current?.querySelector<HTMLElement>("button")?.focus();
    const outside = (e: PointerEvent): void => {
      if (!box.current?.contains(e.target as Node)) props.onClose();
    };
    const key = (e: KeyboardEvent): void => {
      if (e.key === "Escape") props.onClose();
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", key);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("keydown", key);
    };
  }, []);
  const item = (label: string, icon: JSX.Element, act: () => void, danger = false): JSX.Element => (
    <button
      type="button"
      role="menuitem"
      class={danger ? "danger" : undefined}
      onClick={() => {
        act();
        props.onClose();
      }}
    >
      {icon} {label}
    </button>
  );
  // Kept inside the window near the pointer.
  const left = Math.min(props.x, window.innerWidth - 200);
  const top = Math.min(props.y, window.innerHeight - 170);
  return (
    <div
      class="context-menu"
      role="menu"
      aria-label={props.agent.name}
      ref={box}
      style={{ left: `${String(left)}px`, top: `${String(top)}px` }}
    >
      {item("Rename…", <Pencil size={16} />, props.onRename)}
      {item("Move up", <ArrowUp size={16} />, () => {
        props.onStep(-1);
      })}
      {item("Move down", <ArrowDown size={16} />, () => {
        props.onStep(1);
      })}
      {item(`Delete ${props.agent.name}…`, <Trash2 size={16} />, props.onDelete, true)}
    </div>
  );
}

/** A pinned (main) agent: large avatar and name at the top of the list. */
function PinnedAgent(props: {
  agent: Agent;
  active: boolean;
  onSelect: (id: string) => void;
  row: RowHandlers;
}): JSX.Element {
  const a = props.agent;
  const status = statusOf(a);
  const { class: drop, ...row } = props.row;
  return (
    <div class={`pinned-agent-wrap${drop}`} {...row}>
      <button
        type="button"
        class={`pinned-agent${props.active ? " active" : ""}`}
        aria-current={props.active ? "page" : undefined}
        title={`${a.name} · ${status.label}`}
        onClick={() => {
          props.onSelect(a.id);
        }}
      >
        <Avatar agent={a} size={60} presence={status.presence} />
        <span class="pinned-name">{a.name}</span>
        {a.unread > 0 && (
          <span class="badge" aria-label={`${String(a.unread)} unread`}>
            {a.unread > 99 ? "99+" : a.unread}
          </span>
        )}
      </button>
      <PinToggle agent={a} />
    </div>
  );
}

function AgentRow(props: {
  agent: Agent;
  active: boolean;
  onSelect: (id: string) => void;
  row: RowHandlers;
}): JSX.Element {
  const a = props.agent;
  const status = statusOf(a);
  const line = rowLine(a);
  const { class: drop, ...row } = props.row;
  return (
    <div class={`agent-row-wrap${drop}`} {...row}>
      <button
        type="button"
        class={`agent-row${props.active ? " active" : ""}`}
        aria-current={props.active ? "page" : undefined}
        title={[a.name, status.label, a.description].filter(Boolean).join(" · ")}
        onClick={() => {
          props.onSelect(a.id);
        }}
      >
        <Avatar agent={a} size={40} presence={status.presence} />
        <span class="agent-row-text">
          <span class="agent-row-top">
            <span class="agent-name">{a.name}</span>
            <TagChips tags={a.tags} />
            {line.time && <span class="agent-time">{line.time}</span>}
          </span>
          <span class={`agent-desc${line.tone ? ` ${line.tone}` : ""}`}>{line.text}</span>
        </span>
        {a.unread > 0 && (
          <span class="badge" aria-label={`${String(a.unread)} unread`}>
            {a.unread > 99 ? "99+" : a.unread}
          </span>
        )}
      </button>
      <PinToggle agent={a} />
    </div>
  );
}

/** An agent in the collapsed rail: its avatar, a star when pinned and a dot when unread. */
function RailAgent(props: {
  agent: Agent;
  active: boolean;
  onSelect: (id: string) => void;
  onContextMenu: (e: MouseEvent) => void;
}): JSX.Element {
  const a = props.agent;
  const status = statusOf(a);
  return (
    <button
      type="button"
      class={`rail-agent${props.active ? " active" : ""}`}
      aria-label={a.unread > 0 ? `${a.name}, ${String(a.unread)} unread` : a.name}
      aria-current={props.active ? "page" : undefined}
      title={`${a.name} · ${status.label}`}
      onClick={() => {
        props.onSelect(a.id);
      }}
      onContextMenu={props.onContextMenu}
    >
      <Avatar agent={a} size={40} presence={status.presence} />
      {a.pinned && <Star class="rail-star" size={10} aria-hidden="true" />}
      {a.unread > 0 && <span class="rail-unread" aria-hidden="true" />}
    </button>
  );
}
