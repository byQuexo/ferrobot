import type { JSX } from "preact";
import { useMemo, useState } from "preact/hooks";
import { Pin, Plus, Search, Settings } from "lucide-preact";

import type { Agent } from "../api.ts";
import { useApp } from "../context.ts";
import { presenceLabel } from "../lib/format.ts";
import { Avatar } from "./Avatar.tsx";
import { ConversationList, type Pair } from "./Conversations.tsx";
import { IconButton, Logo } from "./ui.tsx";

export function Sidebar(props: {
  selected: string | null;
  onSelect: (id: string) => void;
  pair: Pair | null;
  onOpenPair: (p: Pair) => void;
  onCreate: () => void;
  onSettings: () => void;
  settingsOpen: boolean;
  online: boolean;
}): JSX.Element {
  const { agents } = useApp();
  const [filter, setFilter] = useState("");
  const list = useMemo(() => {
    const f = filter.trim().toLowerCase();
    return [...agents.values()]
      .filter((a) => !f || a.name.includes(f) || a.description.toLowerCase().includes(f))
      // Pinned agents (the main ones) first.
      .sort((a, b) => Number(Boolean(b.pinned)) - Number(Boolean(a.pinned)) || a.name.localeCompare(b.name));
  }, [agents, filter]);

  return (
    <aside class="sidebar">
      <header class="brand">
        <Logo />
        <span class="brand-name">ferrobot</span>
        <span class="spacer" />
        <IconButton label="Settings" onClick={props.onSettings} pressed={props.settingsOpen}>
          <Settings size={17} />
        </IconButton>
        <IconButton label="New agent" onClick={props.onCreate} tone="accent">
          <Plus size={18} />
        </IconButton>
      </header>
      <label class="search">
        <Search size={15} aria-hidden="true" />
        <input
          type="search"
          placeholder="Find an agent"
          value={filter}
          onInput={(e) => { setFilter(e.currentTarget.value); }}
        />
      </label>
      <nav class="agent-list" aria-label="Agents">
        {list.some((a) => a.pinned) && (
          <div class="pinned-strip" aria-label="Pinned agents">
            {list.filter((a) => a.pinned).map((a) => (
              <PinnedAgent key={a.id} agent={a} active={!props.pair && a.id === props.selected} onSelect={props.onSelect} />
            ))}
          </div>
        )}
        {list.filter((a) => !a.pinned).map((a) => (
          <AgentRow key={a.id} agent={a} active={!props.pair && a.id === props.selected} onSelect={props.onSelect} />
        ))}
        {list.length === 0 && agents.size > 0 && <p class="muted pad">No agent matches “{filter}”.</p>}
        <ConversationList active={props.pair} onOpen={props.onOpenPair} />
      </nav>
      <footer class="sidebar-foot">
        <span class={`conn ${props.online ? "on" : "off"}`}>
          <span class="conn-dot" />
          {props.online ? "Daemon connected" : "Daemon offline"}
        </span>
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
      <Pin size={14} />
    </button>
  );
}

/** A pinned (main) agent: large avatar and name at the top of the list. */
function PinnedAgent(props: { agent: Agent; active: boolean; onSelect: (id: string) => void }): JSX.Element {
  const a = props.agent;
  return (
    <div class="pinned-agent-wrap">
      <button
        type="button"
        class={`pinned-agent${props.active ? " active" : ""}`}
        aria-current={props.active ? "page" : undefined}
        title={`${a.name} · ${presenceLabel(a.presence)}`}
        onClick={() => { props.onSelect(a.id); }}
      >
        <Avatar agent={a} size={60} presence={a.presence} />
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

function AgentRow(props: { agent: Agent; active: boolean; onSelect: (id: string) => void }): JSX.Element {
  const a = props.agent;
  return (
    <div class="agent-row-wrap">
    <button
      type="button"
      class={`agent-row${props.active ? " active" : ""}`}
      aria-current={props.active ? "page" : undefined}
      title={`${a.name} · ${presenceLabel(a.presence)}`}
      onClick={() => { props.onSelect(a.id); }}
    >
      <Avatar agent={a} size={40} presence={a.presence} />
      <span class="agent-row-text">
        <span class="agent-row-top">
          <span class="agent-name">{a.name}</span>
        </span>
        <span class="agent-desc">{a.description || "No description"}</span>
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
