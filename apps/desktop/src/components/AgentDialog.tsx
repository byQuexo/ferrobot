import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { ChevronRight, Dices, ImageUp, RotateCcw, X } from "lucide-preact";

import type { Agent, Connector, Provider, ProviderKind, ToolGroup } from "../api.ts";
import { useApp } from "../context.ts";
import {
  BODIES,
  EYES,
  HUES,
  characterOf,
  deriveCharacter,
  rerollCharacter,
  type Character,
} from "../lib/character.ts";
import { addTag } from "../lib/agent-meta.ts";
import { errorText, toolsSummary } from "../lib/format.ts";
import { toAvatarPng } from "../lib/image.ts";
import { AGENT_NAME } from "../lib/welcome.ts";
import { CharacterFace } from "./Character.tsx";
import { Avatar } from "./Avatar.tsx";
import { ModelPicker } from "./ModelPicker.tsx";
import { KIND_LABELS } from "./ProvidersSettings.tsx";
import { Button, Dialog, Switch } from "./ui.tsx";

const TOOL_GROUPS: readonly [ToolGroup, string, string][] = [
  ["team", "Team", "Message teammates and give them tasks"],
  ["memory", "Memory", "Remember and recall notes"],
  [
    "operator",
    "Operator",
    "Ask you questions, share files, bring teammates into calls, read the guides",
  ],
  ["create_agents", "Create agents", "Ask to create new agents"],
  ["add_connectors", "Add connectors", "Ask you to add connectors"],
  ["routines", "Routines", "Set up its own routines"],
  ["shell", "Shell", "Run commands and every CLI on its computer"],
  ["files", "Files", "Read and edit files"],
  ["web", "Web", "Fetch pages and search the web"],
  ["desktop", "Desktop", "See and use its desktop: screenshots, clicks and typing"],
];
// Desktop is opt-in, as in the daemon.
const DEFAULT_TOOLS = TOOL_GROUPS.map(([g]) => g).filter((g) => g !== "desktop");

/** Models driven by ferrobot's own loop for OpenAI-compatible APIs have no web tools. */
function hasWebTools(kind: ProviderKind): boolean {
  return ["anthropic", "bedrock", "vertex", "codex", "gemini_cli"].includes(kind);
}

/** Create (no `agent`) or edit an agent's profile. */
export function AgentDialog(props: {
  agent?: Agent;
  onClose: () => void;
  onSaved: (a: Agent) => void;
}): JSX.Element {
  const { client, toast, health } = useApp();
  const noDaemonKey = health?.driver === "claude" && !health.anthropic_key;
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
  const [tools, setTools] = useState<ToolGroup[]>(editing?.tools ?? DEFAULT_TOOLS);
  const [toolsOpen, setToolsOpen] = useState(false);
  const [tags, setTags] = useState<string[]>(editing?.tags ?? []);
  const [tagText, setTagText] = useState("");
  const [tagError, setTagError] = useState("");
  // The tags with what is still typed in the box; `null` when that is not a valid tag.
  const withTyped = (): string[] | null => {
    if (!tagText.trim()) return tags;
    const next = addTag(tags, tagText);
    if ("error" in next) {
      setTagError(next.error);
      return null;
    }
    return next.tags;
  };
  const commitTag = (): void => {
    const next = withTyped();
    if (next) {
      setTags(next);
      setTagText("");
    }
  };
  const chosen = providers.find((p) => p.id === providerId);
  const groups = TOOL_GROUPS.filter(
    ([g]) => g !== "web" || hasWebTools(chosen?.kind ?? "anthropic"),
  );
  const modelChanged =
    !editing ||
    providerId !== (editing.provider_id ?? null) ||
    model.trim() !== (editing.model ?? "");
  const file = useRef<HTMLInputElement>(null);
  // New agents get a random starting look (saved with the agent); existing
  // ones start from their stored or id-derived look.
  const [seed] = useState(() => crypto.randomUUID());
  const [look, setLook] = useState<Character>(() =>
    editing ? characterOf(editing) : deriveCharacter(seed),
  );
  const same = (a: Character, b: Character): boolean =>
    a.body === b.body && a.eyes === b.eyes && a.hue === b.hue;
  const lookChanged = !editing || !same(look, characterOf(editing));

  useEffect(
    () => () => {
      if (preview) URL.revokeObjectURL(preview);
    },
    [preview],
  );
  useEffect(() => {
    client.listProviders().then(
      (list) => {
        setProviders(list);
        // A new agent on a daemon without its own key needs a provider to reach a model.
        if (!editing && noDaemonKey && list[0]) setProviderId((cur) => cur ?? list[0]?.id ?? null);
      },
      () => undefined,
    );
    client.listConnectors().then(setConnectors, () => undefined);
    if (editing) {
      client.agentConnectors(editing.id).then(
        (ids) => {
          setGiven(ids);
          setPicked(ids);
        },
        () => undefined,
      );
    }
  }, []);
  const togglePick = (id: string): void => {
    setPicked((p) => (p.includes(id) ? p.filter((x) => x !== id) : [...p, id]));
  };

  const nameOk = AGENT_NAME.test(name);
  const imageUrl = avatar === null ? null : (editing?.avatar_url ?? null);
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
    const finalTags = withTyped();
    if (!nameOk || !finalTags) return;
    setSaving(true);
    try {
      let agent = editing
        ? await client.updateAgent(editing.id, {
            ...(name !== editing.name ? { name } : {}),
            description,
            instructions,
            ...(lookChanged
              ? { character: same(look, deriveCharacter(editing.id)) ? null : look }
              : {}),
            ...(modelChanged ? { provider_id: providerId, model: model.trim() || null } : {}),
            tools,
            tags: finalTags,
          })
        : await client.createAgent({
            name,
            description,
            instructions,
            character: look,
            provider_id: providerId,
            model: model.trim() || null,
            tools,
            tags: finalTags,
          });
      if (given && (picked.length !== given.length || picked.some((id) => !given.includes(id)))) {
        await client.setAgentConnectors(agent.id, picked);
      }
      if (avatar instanceof Blob) agent = await client.setAvatar(agent.id, avatar);
      else if (avatar === null && editing?.avatar_url) agent = await client.removeAvatar(agent.id);
      props.onSaved(agent);
    } catch (err) {
      toast(errorText(err));
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
          <Button variant="ghost" onClick={props.onClose}>
            Cancel
          </Button>
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
                      onClick={() => {
                        setLook({ ...look, body });
                      }}
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
                      onClick={() => {
                        setLook({ ...look, eyes });
                      }}
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
                      onClick={() => {
                        setLook({ ...look, hue });
                      }}
                    />
                  ))}
                </div>
              </>
            )}
            <div class="avatar-actions">
              {!hasImage && (
                <Button
                  small
                  onClick={() => {
                    setLook(rerollCharacter(look));
                  }}
                >
                  <Dices size={16} /> Re-roll
                </Button>
              )}
              {!hasImage && editing && !same(look, deriveCharacter(editing.id)) && (
                <Button
                  small
                  variant="ghost"
                  onClick={() => {
                    setLook(deriveCharacter(editing.id));
                  }}
                >
                  <RotateCcw size={16} /> Default
                </Button>
              )}
              <Button small variant="ghost" onClick={() => file.current?.click()}>
                <ImageUp size={16} /> {hasImage ? "Replace image" : "Upload image"}
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
                  <X size={16} /> Use character
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
            onInput={(e) => {
              setName(e.currentTarget.value.toLowerCase());
            }}
          />
          <span class={`hint${name && !nameOk ? " bad" : ""}`}>
            Lowercase letters, digits and dashes; starts with a letter. Other agents can use it to
            message this one.
            {editing && " Renaming keeps the agent’s computer, memory and history."}
          </span>
        </label>
        <label>
          Description
          <input
            value={description}
            maxLength={280}
            placeholder="One line shown in the agent list"
            onInput={(e) => {
              setDescription(e.currentTarget.value);
            }}
          />
        </label>
        <div class="field">
          <label for="agent-tags">Tags</label>
          <span class="tag-editor">
            {tags.map((t) => (
              <span key={t} class="tag-chip">
                {t}
                <button
                  type="button"
                  aria-label={`Remove tag ${t}`}
                  onClick={() => {
                    setTags(tags.filter((x) => x !== t));
                  }}
                >
                  <X size={12} />
                </button>
              </span>
            ))}
            <input
              id="agent-tags"
              value={tagText}
              maxLength={40}
              placeholder={tags.length ? "" : "e.g. research"}
              autoComplete="off"
              onInput={(e) => {
                setTagText(e.currentTarget.value);
                setTagError("");
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === ",") {
                  e.preventDefault();
                  commitTag();
                } else if (e.key === "Backspace" && !tagText && tags.length) {
                  setTags(tags.slice(0, -1));
                }
              }}
            />
          </span>
          <span class={`hint${tagError ? " bad" : ""}`}>
            {tagError || "Up to 5 short labels, shown next to the name. Press Enter to add one."}
          </span>
        </div>
        <label>
          Instructions
          <textarea
            rows={6}
            value={instructions}
            placeholder="Who is this agent and how should it work? For example: You keep the team’s release notes up to date."
            onInput={(e) => {
              setInstructions(e.currentTarget.value);
            }}
          />
          <span class="hint">
            Becomes part of the agent’s system prompt. Changes apply from its next turn.
          </span>
        </label>
        <label>
          Model provider
          <select
            value={providerId ?? ""}
            onChange={(e) => {
              setProviderId(e.currentTarget.value || null);
            }}
          >
            <option value="" disabled={noDaemonKey}>
              {noDaemonKey
                ? "Default (no daemon key: pick a provider)"
                : "Default (the daemon’s Anthropic key)"}
            </option>
            {providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name} · {KIND_LABELS[p.kind]}
              </option>
            ))}
          </select>
        </label>
        <label>
          Model
          <ModelPicker
            provider={providerId ?? undefined}
            kind={chosen?.kind ?? "anthropic"}
            value={model}
            placeholder={
              chosen?.default_model ? `Default (${chosen.default_model})` : "The provider’s default"
            }
            onChange={setModel}
          />
          {editing && modelChanged && (
            <span class="hint">
              Saving restarts the agent’s computer with the new model. Its files are kept.
            </span>
          )}
        </label>
        <div class="field">
          <button
            type="button"
            class="tools-toggle"
            aria-expanded={toolsOpen}
            aria-controls="agent-tools"
            onClick={() => {
              setToolsOpen(!toolsOpen);
            }}
          >
            <ChevronRight size={16} class="tools-chevron" aria-hidden="true" />
            Tools
            <span class="hint">
              {toolsSummary(
                groups.filter(([g]) => tools.includes(g)).length,
                groups.length,
                given ? picked.length : 0,
              )}
            </span>
          </button>
          <div class="field tools-list" id="agent-tools" hidden={!toolsOpen}>
            {groups.map(([group, label, what]) => (
              <span key={group} class="field-row">
                <span>
                  {label} <span class="hint">{what}</span>
                </span>
                <Switch
                  checked={tools.includes(group)}
                  label={label}
                  onChange={(on) => {
                    setTools((t) => (on ? [...t, group] : t.filter((g) => g !== group)));
                  }}
                />
              </span>
            ))}
            {given &&
              connectors.map((c) => (
                <span key={c.id} class="field-row">
                  <span>
                    {c.name} <span class="hint">Connector</span>
                  </span>
                  <Switch
                    checked={picked.includes(c.id)}
                    label={c.name}
                    onChange={() => {
                      togglePick(c.id);
                    }}
                  />
                </span>
              ))}
            <span class="hint">
              Changes apply from its next turn. With Shell on, it can still read files and reach the
              web through commands.
            </span>
          </div>
        </div>
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}
