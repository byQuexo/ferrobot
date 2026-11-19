import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { ImageUp, X } from "lucide-preact";

import type { Agent } from "../api.ts";
import { useApp } from "../context.ts";
import { toAvatarPng } from "../lib/image.ts";
import { Avatar } from "./Avatar.tsx";
import { Button, Dialog } from "./ui.tsx";

const NAME = /^[a-z][a-z0-9-]{0,31}$/;

/** Create (no `agent`) or edit an agent's profile. */
export function AgentDialog(props: { agent?: Agent; onClose: () => void; onSaved: (a: Agent) => void }): JSX.Element {
  const { client, toast } = useApp();
  const editing = props.agent;
  const [name, setName] = useState(editing?.name ?? "");
  const [description, setDescription] = useState(editing?.description ?? "");
  const [instructions, setInstructions] = useState(editing?.instructions ?? "");
  // undefined: unchanged, null: remove, Blob: upload
  const [avatar, setAvatar] = useState<Blob | null | undefined>(undefined);
  const [preview, setPreview] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const file = useRef<HTMLInputElement>(null);

  useEffect(() => () => { if (preview) URL.revokeObjectURL(preview); }, [preview]);

  const nameOk = NAME.test(name);
  const shown = avatar === null ? { name: name || "?", avatar_url: null } : { name: name || "?", avatar_url: editing?.avatar_url ?? null };

  const pick = async (f: File | undefined): Promise<void> => {
    if (!f) return;
    try {
      const png = await toAvatarPng(f);
      setAvatar(png);
      setPreview(URL.createObjectURL(png));
    } catch {
      toast("That file could not be read as an image.");
    }
  };

  const save = async (): Promise<void> => {
    if (!nameOk) return;
    setSaving(true);
    try {
      let agent = editing
        ? await client.updateAgent(editing.id, { description, instructions })
        : await client.createAgent({ name, description, instructions });
      if (avatar instanceof Blob) agent = await client.setAvatar(agent.id, avatar);
      else if (avatar === null && editing?.avatar_url) agent = await client.removeAvatar(agent.id);
      props.onSaved(agent);
    } catch (err) {
      toast(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog
      title={editing ? `Edit ${editing.name}` : "New agent"}
      onClose={props.onClose}
      footer={
        <>
          <Button variant="ghost" onClick={props.onClose}>Cancel</Button>
          <Button variant="primary" disabled={!nameOk || saving} onClick={() => void save()}>
            {saving ? "Saving…" : editing ? "Save changes" : "Create agent"}
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
        <div class="avatar-picker">
          <Avatar agent={shown} size={72} preview={avatar === null ? null : preview} />
          <div class="avatar-actions">
            <Button small onClick={() => file.current?.click()}>
              <ImageUp size={14} /> {editing?.avatar_url || preview ? "Replace image" : "Upload image"}
            </Button>
            {(preview || (editing?.avatar_url && avatar !== null)) && (
              <Button
                small
                variant="ghost"
                onClick={() => {
                  setAvatar(null);
                  setPreview(null);
                }}
              >
                <X size={14} /> Use initials
              </Button>
            )}
            <span class="muted small">PNG, JPEG, WebP or GIF. Cropped to a square.</span>
          </div>
          <input
            ref={file}
            type="file"
            accept="image/png,image/jpeg,image/webp,image/gif"
            hidden
            onChange={(e) => void pick(e.currentTarget.files?.[0])}
          />
        </div>
        <label>
          Name
          <input
            value={name}
            disabled={Boolean(editing)}
            placeholder="e.g. researcher"
            maxLength={32}
            autoComplete="off"
            spellcheck={false}
            onInput={(e) => { setName(e.currentTarget.value.toLowerCase()); }}
          />
          <span class={`hint${name && !nameOk ? " bad" : ""}`}>
            {editing
              ? "Names are permanent: they identify the agent’s computer."
              : "Lowercase letters, digits and dashes; starts with a letter. Other agents use it to message this one."}
          </span>
        </label>
        <label>
          Description
          <input
            value={description}
            maxLength={280}
            placeholder="One line shown in the agent list"
            onInput={(e) => { setDescription(e.currentTarget.value); }}
          />
        </label>
        <label>
          Instructions
          <textarea
            rows={6}
            value={instructions}
            placeholder="Who is this agent and how should it work? For example: You keep the team’s release notes up to date."
            onInput={(e) => { setInstructions(e.currentTarget.value); }}
          />
          <span class="hint">Becomes part of the agent’s system prompt. Changes apply from its next turn.</span>
        </label>
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}
