import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";

import { useApp } from "../context.ts";
import { loadOrder, orderAgents } from "../lib/order.ts";
import { Avatar } from "./Avatar.tsx";
import { SECTIONS, type SettingsSection } from "./Settings.tsx";
import { Dialog } from "./ui.tsx";

interface Item {
  id: string;
  label: string;
  hint: string;
  icon: JSX.Element;
  open: () => void;
}

/** Search over agents (in sidebar order) and settings pages; Enter or a click opens one. */
export function QuickSwitcher(props: {
  onAgent: (id: string) => void;
  onSection: (s: SettingsSection) => void;
  onClose: () => void;
}): JSX.Element {
  const { agents } = useApp();
  const [query, setQuery] = useState("");
  const [at, setAt] = useState(0);
  const q = query.trim().toLowerCase();
  const items: Item[] = [
    ...orderAgents([...agents.values()], loadOrder())
      .filter((a) => a.name.includes(q) || a.description.toLowerCase().includes(q))
      .map((a) => ({
        id: `agent-${a.id}`,
        label: a.name,
        hint: a.description,
        icon: <Avatar agent={a} size={24} />,
        open: () => {
          props.onAgent(a.id);
        },
      })),
    ...SECTIONS.filter(([, label]) => label.toLowerCase().includes(q)).map(
      ([id, label, , Icon]) => ({
        id: `section-${id}`,
        label,
        hint: "Settings",
        icon: <Icon size={16} />,
        open: () => {
          props.onSection(id);
        },
      }),
    ),
  ];
  const active = Math.min(at, items.length - 1);
  useEffect(() => {
    document.getElementById(`switch-${String(active)}`)?.scrollIntoView({ block: "nearest" });
  }, [active]);

  return (
    <Dialog title="Go to" onClose={props.onClose}>
      <input
        role="combobox"
        aria-label="Agent or settings page"
        aria-expanded="true"
        aria-controls="switcher"
        aria-activedescendant={active >= 0 ? `switch-${String(active)}` : undefined}
        placeholder="Agent or settings page"
        spellcheck={false}
        autoComplete="off"
        value={query}
        onInput={(e) => {
          setQuery(e.currentTarget.value);
          setAt(0);
        }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" || e.key === "ArrowUp") {
            e.preventDefault();
            const n = items.length;
            if (n) setAt((active + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
          } else if (e.key === "Enter" && !e.isComposing) {
            e.preventDefault();
            items[active]?.open();
          }
        }}
      />
      <ul class="switcher" id="switcher" role="listbox" aria-label="Results">
        {items.map((item, i) => (
          <li
            key={item.id}
            id={`switch-${String(i)}`}
            role="option"
            aria-selected={i === active}
            onMouseMove={() => {
              setAt(i);
            }}
            onClick={item.open}
          >
            {item.icon}
            <span class="switcher-label">{item.label}</span>
            <span class="muted ellipsis">{item.hint}</span>
          </li>
        ))}
      </ul>
      {items.length === 0 && <p class="muted">Nothing matches “{query}”.</p>}
    </Dialog>
  );
}
