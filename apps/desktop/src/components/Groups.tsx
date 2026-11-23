import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Plus } from "lucide-preact";

import type { Room } from "../api.ts";
import { useApp } from "../context.ts";
import { groupChange } from "../lib/call.ts";
import { Avatar } from "./Avatar.tsx";
import { Button, Dialog, IconButton } from "./ui.tsx";

/** Group chats in the sidebar, with the button that makes one. */
export function GroupList(props: {
  active: number | null;
  onOpen: (id: number) => void;
}): JSX.Element | null {
  const { client, agents, listen } = useApp();
  const [groups, setGroups] = useState<Room[]>([]);
  const [creating, setCreating] = useState(false);
  useEffect(() => {
    const load = (): void => {
      void client.groups().then(setGroups, () => undefined);
    };
    load();
    return listen((e) => {
      if ((e.type === "room" && e.room.name) || e.type === "room_deleted") load();
    });
  }, [client, listen]);
  if (agents.size < 2 && groups.length === 0) return null;
  return (
    <section class="convo-section" aria-label="Groups">
      <h2 class="section-label group-head">
        Groups
        <IconButton
          label="New group"
          onClick={() => {
            setCreating(true);
          }}
        >
          <Plus size={14} />
        </IconButton>
      </h2>
      {groups.map((g) => (
        <button
          key={g.id}
          type="button"
          class={`convo-row${props.active === g.id ? " active" : ""}`}
          onClick={() => {
            props.onOpen(g.id);
          }}
        >
          <span class="pair-avatars">
            {g.members.slice(0, 2).flatMap((id) => {
              const a = agents.get(id);
              return a ? [<Avatar key={id} agent={a} size={22} />] : [];
            })}
          </span>
          <span class="agent-row-text">
            <span class="agent-row-top">
              <span class="agent-name">{g.name}</span>
            </span>
          </span>
        </button>
      ))}
      {creating && (
        <GroupDialog
          onClose={() => {
            setCreating(false);
          }}
          onSaved={(g) => {
            setCreating(false);
            props.onOpen(g.id);
          }}
        />
      )}
    </section>
  );
}

/** Makes a group, or (with `group`) renames it, changes its members or deletes it. */
export function GroupDialog(props: {
  group?: Room;
  onClose: () => void;
  onSaved: (g: Room) => void;
  onDeleted?: () => void;
}): JSX.Element {
  const { client, agents, run } = useApp();
  const group = props.group;
  const [name, setName] = useState(group?.name ?? "");
  const [members, setMembers] = useState(group?.members.filter((id) => agents.has(id)) ?? []);
  const [deleting, setDeleting] = useState(false);
  const ready = name.trim() !== "" && members.length >= 2;
  const save = async (): Promise<void> => {
    if (!ready) return;
    const saved = await run(() =>
      group
        ? client.updateGroup(group.id, groupChange(group.members, name, members))
        : client.createGroup(name.trim(), members),
    );
    if (saved) props.onSaved(saved);
  };
  const remove = async (): Promise<void> => {
    if (!group) return;
    if ((await run(() => client.deleteGroup(group.id).then(() => true))) && props.onDeleted)
      props.onDeleted();
  };
  return (
    <Dialog
      title={group ? "Edit group" : "New group"}
      onClose={props.onClose}
      footer={
        <>
          {group && (
            <Button
              variant="danger"
              onClick={() => {
                if (deleting) void remove();
                else setDeleting(true);
              }}
            >
              {deleting ? "Delete group and messages" : "Delete"}
            </Button>
          )}
          <span class="spacer" />
          <Button onClick={props.onClose}>Cancel</Button>
          <Button variant="primary" type="submit" disabled={!ready} onClick={() => void save()}>
            {group ? "Save" : "Create"}
          </Button>
        </>
      }
    >
      <form
        class="form"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <label>
          Name
          <input
            value={name}
            maxLength={64}
            placeholder="e.g. Release team"
            onInput={(e) => {
              setName(e.currentTarget.value);
            }}
          />
        </label>
        <fieldset class="group-members">
          <legend>Members (two or more)</legend>
          {[...agents.values()].map((a) => (
            <label key={a.id} class="check">
              <input
                type="checkbox"
                checked={members.includes(a.id)}
                onChange={(e) => {
                  const on = e.currentTarget.checked;
                  setMembers((m) => (on ? [...m, a.id] : m.filter((id) => id !== a.id)));
                }}
              />
              <Avatar agent={a} size={20} />
              {a.name}
            </label>
          ))}
        </fieldset>
        <span class="hint">
          Everyone in the group reads every message and answers in turn. A call fits four agents.
        </span>
      </form>
    </Dialog>
  );
}
