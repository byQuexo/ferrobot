import type { CSSProperties, JSX } from "preact";
import { useRef, useState } from "preact/hooks";
import { FileText, MessageSquare, Pencil, Pin as PinIcon, PinOff, X } from "lucide-preact";

import type { Pin } from "../api.ts";
import { useApp } from "../context.ts";
import { movePin, pinKey } from "../lib/pins.ts";
import { Avatar } from "./Avatar.tsx";
import { IconButton, useDismiss } from "./ui.tsx";

/** The sidebar's pin button: pinned agents, replies and files, one click away. */
export function PinsMenu(): JSX.Element {
  const { agents, pins, savePins, openAgent, openFile, jumpTo, client, run } = useApp();
  const [open, setOpen] = useState(false);
  // Fixed, because the sidebar clips what overflows it: below the button, or beside it in the rail.
  const [place, setPlace] = useState<CSSProperties>({});
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const dragged = useRef<string | null>(null);
  const wrap = useRef<HTMLSpanElement>(null);
  useDismiss(wrap, open, (escape) => {
    if (editing) return;
    setOpen(false);
    if (escape) wrap.current?.querySelector("button")?.focus();
  });

  const pinnedAgents = [...agents.values()].filter((a) => a.pinned);
  const go = (p: Pin): void => {
    setOpen(false);
    if (p.kind === "file") {
      openAgent(p.agent_id);
      openFile(p.agent_id, p.event);
    } else jumpTo(p.agent_id, p.event);
  };
  const rename = (p: Pin, value: string): void => {
    const label = value.trim();
    setEditing(null);
    if (!label || label === p.label) return;
    void savePins(pins.map((x) => (pinKey(x) === pinKey(p) ? { ...x, label } : x)));
  };

  return (
    <span class="pins-wrap" ref={wrap}>
      <IconButton
        label="Pins"
        expanded={open}
        onClick={() => {
          const r = wrap.current?.querySelector("button")?.getBoundingClientRect();
          if (r)
            setPlace(
              wrap.current?.closest(".rail")
                ? { left: r.right + 8, bottom: window.innerHeight - r.bottom }
                : { left: Math.max(8, r.right - 300), top: r.bottom + 8 },
            );
          setOpen(!open);
        }}
      >
        <PinIcon size={18} />
      </IconButton>
      {open && (
        <div class="pins-pop" role="dialog" aria-label="Pins" style={place}>
          {pinnedAgents.length === 0 && pins.length === 0 && (
            <p class="muted small pins-empty">
              Nothing pinned yet. Pin a reply or a file with its pin button, or an agent from its
              row in the sidebar.
            </p>
          )}
          {pinnedAgents.length > 0 && (
            <section>
              <h3 class="pins-head">Agents</h3>
              <ul class="pins-list">
                {pinnedAgents.map((a) => (
                  <li key={a.id} class="pin-row">
                    <button
                      type="button"
                      class="pin-go"
                      onClick={() => {
                        setOpen(false);
                        openAgent(a.id);
                      }}
                    >
                      <Avatar agent={a} size={20} />
                      <span class="ellipsis">{a.name}</span>
                    </button>
                    <IconButton
                      label={`Unpin ${a.name}`}
                      onClick={() => void run(() => client.updateAgent(a.id, { pinned: false }))}
                    >
                      <PinOff size={14} />
                    </IconButton>
                  </li>
                ))}
              </ul>
            </section>
          )}
          {pins.length > 0 && (
            <section>
              <h3 class="pins-head">Replies and files</h3>
              <ul class="pins-list">
                {pins.map((p) => {
                  const key = pinKey(p);
                  const owner = agents.get(p.agent_id);
                  return (
                    <li
                      key={key}
                      class="pin-row"
                      draggable={editing !== key}
                      onDragStart={(e) => {
                        dragged.current = key;
                        e.dataTransfer?.setData("text/plain", key);
                      }}
                      onDragOver={(e) => {
                        e.preventDefault();
                      }}
                      onDrop={(e) => {
                        e.preventDefault();
                        const from = dragged.current;
                        dragged.current = null;
                        if (!from || from === key) return;
                        const box = e.currentTarget.getBoundingClientRect();
                        const after = e.clientY > box.top + box.height / 2;
                        void savePins(movePin(pins, from, key, after));
                      }}
                    >
                      {editing === key ? (
                        <input
                          class="pin-rename"
                          aria-label="Pin name"
                          value={draft}
                          maxLength={120}
                          ref={(el) => el?.focus()}
                          onInput={(e) => {
                            setDraft(e.currentTarget.value);
                          }}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") rename(p, e.currentTarget.value);
                            if (e.key === "Escape") {
                              e.stopPropagation();
                              setEditing(null);
                            }
                          }}
                          onBlur={(e) => {
                            rename(p, e.currentTarget.value);
                          }}
                        />
                      ) : (
                        <button
                          type="button"
                          class="pin-go"
                          title={p.kind === "file" ? "Open the file" : "Show in the chat"}
                          onClick={() => {
                            go(p);
                          }}
                        >
                          {p.kind === "file" ? <FileText size={16} /> : <MessageSquare size={16} />}
                          <span class="pin-text">
                            <span class="ellipsis">{p.label}</span>
                            {owner && <span class="muted small ellipsis">{owner.name}</span>}
                          </span>
                        </button>
                      )}
                      <IconButton
                        label={`Rename ${p.label}`}
                        onClick={() => {
                          setDraft(p.label);
                          setEditing(key);
                        }}
                      >
                        <Pencil size={14} />
                      </IconButton>
                      <IconButton
                        label={`Unpin ${p.label}`}
                        onClick={() => void savePins(pins.filter((x) => pinKey(x) !== key))}
                      >
                        <X size={14} />
                      </IconButton>
                    </li>
                  );
                })}
              </ul>
              <p class="muted small pins-hint">Drag to reorder.</p>
            </section>
          )}
        </div>
      )}
    </span>
  );
}
