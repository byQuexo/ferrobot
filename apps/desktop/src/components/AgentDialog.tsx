import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { Check, Dices, ImageUp, RotateCcw, X } from "lucide-preact";

import type { Agent, Connector, Provider } from "../api.ts";
import { useApp } from "../context.ts";
import { BODIES, EYES, HUES, characterOf, deriveCharacter, rerollCharacter, type Character } from "../lib/character.ts";
import { toAvatarPng } from "../lib/image.ts";
import { CharacterFace } from "./Character.tsx";
import { Avatar } from "./Avatar.tsx";
import { KIND_LABELS } from "./ProvidersSettings.tsx";
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
  const [providers, setProviders] = useState<Provider[]>([]);
  const [providerId, setProviderId] = useState<string | null>(editing?.provider_id ?? null);
  const [model, setModel] = useState(editing?.model ?? "");
  const [connectors, setConnectors] = useState<Connector[]>([]);
  const [given, setGiven] = useState<string[] | null>(editing ? null : []);
  const [picked, setPicked] = useState<string[]>([]);
  const chosen = providers.find((p) => p.id === providerId);
  const modelChanged =
    !editing || providerId !== (editing.provider_id ?? null) || model.trim() !== (editing.model ?? "");
  const file = useRef<HTMLInputElement>(null);
  // New agents get a random starting look (saved with the agent); existing
  // ones start from their stored or id-derived look.
  const [seed] = useState(() => crypto.randomUUID());
  const [look, setLook] = useState<Character>(() => (editing ? characterOf(editing) : deriveCharacter(seed)));
  const same = (a: Character, b: Character): boolean => a.body === b.body && a.eyes === b.eyes && a.hue === b.hue;
  const lookChanged = !editing || !same(look, characterOf(editing));

  useEffect(() => () => { if (preview) URL.revokeObjectURL(preview); }, [preview]);
  useEffect(() => {
    client.listProviders().then(setProviders, () => undefined);
    client.listConnectors().then(setConnectors, () => undefined);
    if (editing) {
      client.agentConnectors(editing.id).then((ids) => { setGiven(ids); setPicked(ids); }, () => undefined);
    }
  }, []);
  const togglePick = (id: string): void => {
    setPicked((p) => (p.includes(id) ? p.filter((x) => x !== id) : [...p, id]));
  };

  const nameOk = NAME.test(name);
  const imageUrl = avatar === null ? null : editing?.avatar_url ?? null;
  const shown = { name: name || "?", avatar_url: imageUrl, character: look };
  const hasImage = Boolean(preview ?? imageUrl);

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
        ? await client.updateAgent(editing.id, {
            ...(name !== editing.name ? { name } : {}),
            description,
            instructions,
            ...(lookChanged ? { character: same(look, deriveCharacter(editing.id)) ? null : look } : {}),
            ...(modelChanged ? { provider_id: providerId, model: model.trim() || null } : {}),
          })
        : await client.createAgent({
            name,
            description,
            instructions,
            character: look,
            provider_id: providerId,
            model: model.trim() || null,
          });
      if (given && (picked.length !== given.length || picked.some((id) => !given.includes(id)))) {
        await client.setAgentConnectors(agent.id, picked);
      }
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
          <Avatar agent={shown} size={84} preview={avatar === null ? null : preview} />
          <div class="char-editor">
            {hasImage ? (
              <span class="muted small">The uploaded image is shown instead of the character.</span>
            ) : (
              <>
                <div class="char-options" role="group" aria-label="Shape">
                  <span class="char-label">Shape</span>
                  {BODIES.map((body) => (
                    <button
                      key={body}
                      type="button"
                      class={`char-option${look.body === body ? " on" : ""}`}
                      aria-pressed={look.body === body}
                      title={`Body: ${body}`}
                      onClick={() => { setLook({ ...look, body }); }}
                    >
                      <CharacterFace look={{ ...look, body }} mood="idle" blinkDelay={0} />
                    </button>
                  ))}
                </div>
                <div class="char-options" role="group" aria-label="Eyes">
                  <span class="char-label">Eyes</span>
                  {EYES.map((eyes) => (
                    <button
                      key={eyes}
                      type="button"
                      class={`char-option${look.eyes === eyes ? " on" : ""}`}
                      aria-pressed={look.eyes === eyes}
                      title={`Eyes: ${eyes}`}
                      onClick={() => { setLook({ ...look, eyes }); }}
                    >
                      <CharacterFace look={{ ...look, eyes }} mood="idle" blinkDelay={0} />
                    </button>
                  ))}
                </div>
                <div class="char-swatches" role="group" aria-label="Color">
                  <span class="char-label">Color</span>
                  {HUES.map((hue) => (
                    <button
                      key={hue}
                      type="button"
                      class={`swatch${look.hue === hue ? " on" : ""}`}
                      style={{ "--char-hue": String(hue) }}
                      aria-label={`Color ${String(hue)}`}
                      aria-pressed={look.hue === hue}
                      onClick={() => { setLook({ ...look, hue }); }}
                    />
                  ))}
                </div>
              </>
            )}
            <div class="avatar-actions">
              {!hasImage && (
                <Button small onClick={() => { setLook(rerollCharacter(look)); }}>
                  <Dices size={14} /> Re-roll
                </Button>
              )}
              {!hasImage && editing && !same(look, deriveCharacter(editing.id)) && (
                <Button small variant="ghost" onClick={() => { setLook(deriveCharacter(editing.id)); }}>
                  <RotateCcw size={14} /> Default
                </Button>
              )}
              <Button small variant="ghost" onClick={() => file.current?.click()}>
                <ImageUp size={14} /> {hasImage ? "Replace image" : "Upload image"}
              </Button>
              {hasImage && (
                <Button
                  small
                  variant="ghost"
                  onClick={() => {
                    setAvatar(null);
                    setPreview(null);
                  }}
                >
                  <X size={14} /> Use character
                </Button>
              )}
            </div>
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
            placeholder="e.g. researcher"
            maxLength={32}
            autoComplete="off"
            spellcheck={false}
            onInput={(e) => { setName(e.currentTarget.value.toLowerCase()); }}
          />
          <span class={`hint${name && !nameOk ? " bad" : ""}`}>
            Lowercase letters, digits and dashes; starts with a letter. Other agents can use it to message this one.
            {editing && " Renaming keeps the agent’s computer, memory and history."}
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
        <label>
          Model provider
          <select value={providerId ?? ""} onChange={(e) => { setProviderId(e.currentTarget.value || null); }}>
            <option value="">Default (the daemon’s Anthropic key)</option>
            {providers.map((p) => (
              <option key={p.id} value={p.id}>{p.name} · {KIND_LABELS[p.kind]}</option>
            ))}
          </select>
        </label>
        <label>
          Model
          <input
            value={model}
            maxLength={200}
            spellcheck={false}
            placeholder={chosen?.default_model ?? "The provider’s default"}
            onInput={(e) => { setModel(e.currentTarget.value); }}
          />
          {editing && modelChanged && (
            <span class="hint">Saving restarts the agent’s computer with the new model. Its files are kept.</span>
          )}
        </label>
        {connectors.length > 0 && given && (
          <div class="field">
            <span>Connectors</span>
            <div class="qcard-choices">
              {connectors.map((c) => {
                const on = picked.includes(c.id);
                return (
                  <button
                    key={c.id}
                    type="button"
                    class={`choice multi${on ? " on" : ""}`}
                    role="checkbox"
                    aria-checked={on}
                    onClick={() => { togglePick(c.id); }}
                  >
                    <span class="choice-mark">{on && <Check size={12} />}</span>
                    {c.name}
                  </button>
                );
              })}
            </div>
            <span class="hint">The agent can use these connectors’ tools from its next turn.</span>
          </div>
        )}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}
