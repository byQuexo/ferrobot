import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Pencil, Plus, SquareSlash, Trash2 } from "lucide-preact";

import type { SlashCommand } from "../api.ts";
import { useApp } from "../context.ts";
import { TASK_COMMAND } from "../lib/commands.ts";
import { Button, Dialog, IconButton, Switch } from "./ui.tsx";

/** Slash commands for the message box: `/task`, then the operator's own. */
export function CommandsSettings(): JSX.Element | null {
  const { client, run, agents, formerNames } = useApp();
  const [list, setList] = useState<SlashCommand[] | null>(null);
  // `null`: closed; `-1`: a new command; else the index being edited.
  const [editing, setEditing] = useState<number | null>(null);
  useEffect(() => {
    void run(() => client.commands()).then((l) => {
      setList(l ?? []);
    });
  }, []);
  if (!list) return null;

  const save = async (next: SlashCommand[]): Promise<boolean> => {
    const saved = await run(() => client.setCommands(next));
    if (saved) setList(saved);
    return saved !== undefined;
  };
  const by = (id: string): string =>
    agents.get(id)?.name ?? `${formerNames.get(id) ?? "an agent"} (deleted)`;

  return (
    <section class="settings-section">
      <header class="hook-head">
        <div>
          <p class="pref-sub">
            Type <code>/</code> at the start of a message to pick one. <code>/name more text</code>{" "}
            sends the command’s prompt with your text in place of <code>$ARGUMENTS</code>.
          </p>
        </div>
        <Button
          small
          variant="primary"
          onClick={() => {
            setEditing(-1);
          }}
        >
          <Plus size={16} /> New
        </Button>
      </header>
      <ul class="conn-list">
        {[TASK_COMMAND, ...list].map((c, i) => {
          const own = i > 0;
          return (
            <li key={c.name} class="conn-card">
              <span class="conn-tile hook-tile" aria-hidden="true">
                <SquareSlash size={18} />
              </span>
              <div class="conn-main">
                <div class="conn-title">
                  <b>/{c.name}</b>
                  {!own && <span class="hook-chip">Built in</span>}
                  {c.as_task === true && <span class="hook-chip">Runs as a task</span>}
                </div>
                <span class="muted small">{c.description}</span>
                {c.added_by && <span class="muted small">Added by @{by(c.added_by)}</span>}
              </div>
              {own && (
                <div class="conn-actions">
                  <IconButton
                    label={`Edit /${c.name}`}
                    onClick={() => {
                      setEditing(i - 1);
                    }}
                  >
                    <Pencil size={16} />
                  </IconButton>
                  <IconButton
                    label={`Delete /${c.name}`}
                    tone="danger"
                    onClick={() => void save(list.filter((_, n) => n !== i - 1))}
                  >
                    <Trash2 size={16} />
                  </IconButton>
                </div>
              )}
            </li>
          );
        })}
      </ul>
      {editing !== null && (
        <CommandDialog
          command={list[editing]}
          onClose={() => {
            setEditing(null);
          }}
          onSave={async (c) => {
            const next =
              editing === -1 ? [...list, c] : list.map((old, n) => (n === editing ? c : old));
            if (await save(next)) setEditing(null);
          }}
        />
      )}
    </section>
  );
}

function CommandDialog(props: {
  command: SlashCommand | undefined;
  onClose: () => void;
  onSave: (c: SlashCommand) => Promise<void>;
}): JSX.Element {
  const old = props.command;
  const [name, setName] = useState(old?.name ?? "");
  const [description, setDescription] = useState(old?.description ?? "");
  const [prompt, setPrompt] = useState(old?.prompt ?? "");
  const [asTask, setAsTask] = useState(old?.as_task === true);
  const [saving, setSaving] = useState(false);
  const ready = /^[a-z][a-z0-9-]*$/.test(name) && description.trim() && prompt.trim();
  const submit = async (): Promise<void> => {
    setSaving(true);
    await props.onSave({
      name,
      description: description.trim(),
      prompt,
      as_task: asTask,
      added_by: old?.added_by ?? null,
    });
    setSaving(false);
  };
  return (
    <Dialog
      title={old ? `Edit /${old.name}` : "New command"}
      onClose={props.onClose}
      footer={
        <>
          <Button variant="ghost" onClick={props.onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!ready || saving} onClick={() => void submit()}>
            {old ? "Save" : "Add command"}
          </Button>
        </>
      }
    >
      <div class="form">
        <label>
          Name
          <input
            value={name}
            maxLength={32}
            spellcheck={false}
            autoComplete="off"
            placeholder="e.g. weekly-report"
            onInput={(e) => {
              setName(e.currentTarget.value.toLowerCase());
            }}
          />
          <span class="hint">Lowercase letters, digits and dashes, starting with a letter.</span>
        </label>
        <label>
          Description
          <input
            value={description}
            maxLength={100}
            placeholder="e.g. Sum up the week"
            onInput={(e) => {
              setDescription(e.currentTarget.value);
            }}
          />
          <span class="hint">Shown in the menu when you type /.</span>
        </label>
        <label>
          Prompt
          <textarea
            rows={6}
            value={prompt}
            maxLength={4000}
            placeholder="e.g. Write the weekly report for $ARGUMENTS."
            onInput={(e) => {
              setPrompt(e.currentTarget.value);
            }}
          />
          <span class="hint">
            <code>$ARGUMENTS</code> becomes what you type after the command; without it, your text
            follows after a blank line.
          </span>
        </label>
        <span class="field">
          <span class="field-row">
            Run as a task
            <Switch checked={asTask} label="Run as a task" onChange={setAsTask} />
          </span>
          <span class="hint">
            The agent keeps working until the goal is reached, checked after every turn.
          </span>
        </span>
      </div>
    </Dialog>
  );
}
