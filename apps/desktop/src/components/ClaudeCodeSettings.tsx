import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Trash2 } from "lucide-preact";

import type { ClaudeCodeSettings as Settings, HookEvent, InstalledPlugin } from "../api.ts";
import { useApp } from "../context.ts";
import { PrefRow } from "./AppearanceSettings.tsx";
import { Button, IconButton, Switch } from "./ui.tsx";

const EVENTS: [HookEvent, string][] = [
  ["PreToolUse", "Before a tool call"],
  ["PostToolUse", "After a tool call"],
  ["UserPromptSubmit", "When a turn starts"],
  ["Stop", "When a turn ends"],
  ["SessionStart", "When a session starts"],
  ["SessionEnd", "When a session ends"],
];

const perTool = (event: HookEvent): boolean => event === "PreToolUse" || event === "PostToolUse";

/** Hooks and plugins for agents on Claude Code (your own login); saved as soon as they change. */
export function ClaudeCodeSettings(): JSX.Element | null {
  const { client, run } = useApp();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [plugins, setPlugins] = useState<InstalledPlugin[] | null>(null);
  const [event, setEvent] = useState<HookEvent>("PreToolUse");
  const [matcher, setMatcher] = useState("");
  const [command, setCommand] = useState("");

  useEffect(() => {
    void run(() => client.claudeCodeSettings()).then((s) => {
      setSettings(s ?? null);
    });
    client.claudeCodePlugins().then(setPlugins, () => {
      setPlugins([]);
    });
  }, []);

  if (!settings) return null;
  const save = async (next: Settings): Promise<boolean> => {
    const saved = await run(() => client.updateClaudeCodeSettings(next));
    if (saved) setSettings(saved);
    return Boolean(saved);
  };
  const addHook = async (): Promise<void> => {
    const hook = {
      event,
      matcher: perTool(event) && matcher.trim() ? matcher.trim() : null,
      command,
    };
    if (await save({ ...settings, hooks: [...settings.hooks, hook] })) {
      setMatcher("");
      setCommand("");
    }
  };
  const label = (e: HookEvent): string => EVENTS.find(([id]) => id === e)?.[1] ?? e;

  return (
    <section class="settings-section">
      <p class="hint">
        For agents on Claude Code (your own login). Hooks and plugins run on this computer, outside
        the agents’ sandboxes. Your own Claude Code settings stay out, and agents cannot change
        these.
      </p>

      <ul class="pref-list">
        <PrefRow
          title="Subagents"
          hint="Claude Code’s own subagents. They get the same ferrobot tools, so they work in the agent’s computer too, and use more of your plan."
        >
          <Switch
            checked={settings.subagents}
            label="Subagents"
            onChange={(v) => void save({ ...settings, subagents: v })}
          />
        </PrefRow>
      </ul>

      <h3 class="pref-head">Hooks</h3>
      <ul class="pref-list">
        {settings.hooks.map((h, i) => (
          <PrefRow
            key={`${i}-${h.command}`}
            title={label(h.event)}
            hint={h.matcher ? `Tools: ${h.matcher}` : undefined}
          >
            <code class="hook-command">{h.command}</code>
            <IconButton
              label={`Remove hook ${h.command}`}
              tone="danger"
              onClick={() =>
                void save({ ...settings, hooks: settings.hooks.filter((_, j) => j !== i) })
              }
            >
              <Trash2 size={16} />
            </IconButton>
          </PrefRow>
        ))}
        <li class="pref-row hook-add">
          <select
            aria-label="When"
            value={event}
            onChange={(e) => {
              setEvent(e.currentTarget.value as HookEvent);
            }}
          >
            {EVENTS.map(([id, text]) => (
              <option key={id} value={id}>
                {text}
              </option>
            ))}
          </select>
          {perTool(event) && (
            <input
              aria-label="Tools"
              value={matcher}
              maxLength={200}
              spellcheck={false}
              autoComplete="off"
              placeholder="Tools, empty for all"
              onInput={(e) => {
                setMatcher(e.currentTarget.value);
              }}
            />
          )}
          <input
            aria-label="Command"
            class="hook-command-input"
            value={command}
            maxLength={4096}
            spellcheck={false}
            autoComplete="off"
            placeholder="Command, e.g. ~/bin/log-tool-call.sh"
            onInput={(e) => {
              setCommand(e.currentTarget.value);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && command.trim()) void addHook();
            }}
          />
          <Button small disabled={!command.trim()} onClick={() => void addHook()}>
            Add
          </Button>
        </li>
      </ul>
      <p class="hint">
        A hook gets the event as JSON on stdin. Exit code 2 on “Before a tool call” blocks the call.
      </p>

      <h3 class="pref-head">Plugins</h3>
      {plugins === null ? null : plugins.length === 0 ? (
        <p class="hint">
          No plugins installed. Install one with <code>claude plugin install</code>.
        </p>
      ) : (
        <ul class="pref-list">
          {plugins.map((p) => {
            const on = settings.plugins.includes(p.id);
            return (
              <PrefRow key={p.id} title={p.id} hint={p.version ?? undefined}>
                <Switch
                  checked={on}
                  label={p.id}
                  onChange={(v) =>
                    void save({
                      ...settings,
                      plugins: v
                        ? [...settings.plugins, p.id]
                        : settings.plugins.filter((id) => id !== p.id),
                    })
                  }
                />
              </PrefRow>
            );
          })}
        </ul>
      )}
      <p class="hint">
        A plugin’s hooks run, and its subagents when Subagents is on. Its skills and commands need
        Claude Code’s built-in tools, which stay off. Its MCP servers don’t start: add them under
        Connectors as a local server.
      </p>
    </section>
  );
}
