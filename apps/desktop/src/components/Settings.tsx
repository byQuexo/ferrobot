import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import {
  AudioLines,
  BookOpen,
  Bot,
  Brain,
  Code,
  Cpu,
  Keyboard,
  KeyRound,
  Palette,
  Plug,
  RefreshCw,
  Stethoscope,
  Terminal,
  Waypoints,
  Webhook,
  X,
} from "lucide-preact";

import type { GeneralSettings } from "../api.ts";
import { useApp } from "../context.ts";
import type { Appearance } from "../lib/appearance.ts";
import { isMac, keyLabel, SHORTCUTS } from "../lib/shortcuts.ts";
import { AppearanceSettings, PrefRow } from "./AppearanceSettings.tsx";
import { ClaudeCodeSettings } from "./ClaudeCodeSettings.tsx";
import { ClisSettings } from "./ClisSettings.tsx";
import { ConnectorsSettings } from "./ConnectorsSettings.tsx";
import { DiagnosticsSettings } from "./DiagnosticsSettings.tsx";
import { Guides } from "./Guides.tsx";
import { Memory } from "./InfoPanel.tsx";
import { MemoryVault } from "./MemoryVault.tsx";
import { ProvidersSettings } from "./ProvidersSettings.tsx";
import { TracesSettings } from "./TracesSettings.tsx";
import { UpdatesSettings } from "./UpdatesSettings.tsx";
import { VaultSettings } from "./VaultSettings.tsx";
import { VoiceSettings } from "./VoiceSettings.tsx";
import { WebhooksSettings } from "./WebhooksSettings.tsx";
import { IconButton, Switch } from "./ui.tsx";

export type SettingsSection =
  | "providers"
  | "vault"
  | "agents"
  | "claude-code"
  | "connectors"
  | "webhooks"
  | "clis"
  | "memory"
  | "traces"
  | "voice"
  | "appearance"
  | "keyboard"
  | "updates"
  | "diagnostics"
  | "guides";

/** Id, label, about, icon and the guide page behind the header's "Guide" link. */
export const SECTIONS: [SettingsSection, string, string, typeof KeyRound, string?][] = [
  ["appearance", "Appearance", "How ferrobot looks on this computer.", Palette],
  ["keyboard", "Keyboard", "Shortcuts that work anywhere in the app.", Keyboard],
  [
    "voice",
    "Voice",
    "Agents’ replies read aloud and spoken messages. Start a call with the phone button in a chat.",
    AudioLines,
  ],
  ["providers", "Model providers", "The models your agents run on. Their keys live in Vault.", Cpu],
  ["agents", "Agents", "What agents may do without asking you.", Bot],
  [
    "claude-code",
    "Claude Code",
    "Hooks and plugins for agents that run on Claude Code.",
    Code,
    "Providers",
  ],
  [
    "memory",
    "Global memory",
    "Facts every agent can recall. Agents add to it; you can edit and remove entries.",
    Brain,
  ],
  [
    "traces",
    "Traces",
    "Send each turn’s trace to Phoenix, Langfuse or any OpenTelemetry backend.",
    Waypoints,
    "Traces",
  ],
  [
    "connectors",
    "Connectors",
    "Remote MCP servers whose tools you can give to agents, with a token or an OAuth sign-in.",
    Plug,
    "Connectors-and-OAuth",
  ],
  [
    "webhooks",
    "Webhooks",
    "Messages from other services to your agents, and your agents’ events to Discord, Slack or any URL.",
    Webhook,
    "Webhooks",
  ],
  [
    "clis",
    "CLIs",
    "Command-line tools in every agent’s computer, built into one shared image.",
    Terminal,
  ],
  [
    "vault",
    "Vault",
    "Every key in one place: model providers, connectors and voice. Kept in the keychain of the daemon’s computer; agents never see them.",
    KeyRound,
  ],
  ["updates", "Updates", "The version ferrobot runs and how to get the latest.", RefreshCw],
  [
    "diagnostics",
    "Diagnostics",
    "Versions, status and the daemon log, to help with problems and bug reports.",
    Stethoscope,
    "Troubleshooting",
  ],
  ["guides", "Guides", "How ferrobot works, step by step. The same pages as the wiki.", BookOpen],
];

/** The sidebar's categories, in order; together they hold every section once. */
const GROUPS: [string, SettingsSection[]][] = [
  ["General", ["appearance", "keyboard", "voice"]],
  ["Agents", ["providers", "agents", "claude-code", "memory", "traces"]],
  ["Tools & keys", ["connectors", "webhooks", "clis", "vault"]],
  ["App", ["updates", "diagnostics", "guides"]],
];

/** Auto mode; saved as soon as it is switched. */
function AgentSettings(): JSX.Element | null {
  const { client, run } = useApp();
  const [settings, setSettings] = useState<GeneralSettings | null>(null);
  const [names, setNames] = useState("");

  useEffect(() => {
    const load = async (): Promise<void> => {
      const loaded = (await run(() => client.settings())) ?? null;
      setSettings(loaded);
      setNames(loaded?.agent_names ?? "");
    };
    void load();
  }, []);

  if (!settings) return null;
  const save = async (changes: Partial<GeneralSettings>): Promise<void> => {
    const saved = await run(() => client.updateSettings({ ...settings, ...changes }));
    if (saved) {
      setSettings(saved);
      setNames(saved.agent_names ?? "");
    }
  };
  const setAuto = (auto_mode: boolean): Promise<void> => save({ auto_mode });
  return (
    <section class="settings-section form">
      <span class="field">
        <span class="field-row">
          Auto mode
          <Switch
            checked={settings.auto_mode}
            label="Auto mode"
            onChange={(v) => void setAuto(v)}
          />
        </span>
        <span class="hint">
          Agents create teammates and take risky steps (deleting folders, pushing code, sending
          through a connector) without asking first. Without it, you get a card for each risky step.
          Dangerous commands stay blocked, and the agent limit and keys stay protected.
        </span>
      </span>
      <label>
        Names for new agents
        <input
          value={names}
          maxLength={300}
          placeholder="e.g. Greek gods"
          onInput={(e) => {
            setNames(e.currentTarget.value);
          }}
          onBlur={() => {
            if (names.trim() !== (settings.agent_names ?? ""))
              void save({ agent_names: names.trim() || null });
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
        />
        <span class="hint">
          How agents name the agents they create. Empty: short names that fit the role, like scout.
        </span>
      </label>
    </section>
  );
}

/** Every keyboard shortcut with its keys on this platform. */
export function ShortcutList(): JSX.Element {
  const mac = isMac();
  return (
    <ul class="pref-list">
      {SHORTCUTS.map((s) => (
        <PrefRow key={s.id} title={s.label}>
          <kbd>{keyLabel(s, mac)}</kbd>
        </PrefRow>
      ))}
    </ul>
  );
}

/** All app-wide settings on one page. */
export function Settings(props: {
  section: SettingsSection;
  onSection: (s: SettingsSection) => void;
  onClose: () => void;
  look: Appearance;
  onLook: (look: Appearance) => void;
  /** The open guide under Guides; `null`: the list. */
  guide: string | null;
  onGuide: (page: string | null) => void;
}): JSX.Element {
  const [, title, about, , guide] = SECTIONS.find(([id]) => id === props.section) ?? [
    "providers",
    "Settings",
    "",
    KeyRound,
  ];
  const [vaultFocus, setVaultFocus] = useState<string>();
  const vault = (): void => {
    props.onSection("vault");
  };
  const vaultAt = (focus?: string): void => {
    setVaultFocus(focus);
    vault();
  };
  return (
    <main class="settings" aria-label="Settings">
      <nav class="settings-nav" aria-label="Settings sections">
        <h1>Settings</h1>
        {GROUPS.map(([group, ids]) => [
          <h2 key={group} class="settings-group">
            {group}
          </h2>,
          ...SECTIONS.filter(([id]) => ids.includes(id)).map(([id, label, , Icon]) => (
            <button
              key={id}
              type="button"
              class={`settings-tab${props.section === id ? " active" : ""}`}
              aria-current={props.section === id ? "page" : undefined}
              onClick={() => {
                setVaultFocus(undefined);
                props.onSection(id);
              }}
            >
              <Icon size={16} /> {label}
            </button>
          )),
        ])}
      </nav>
      <section class="settings-body">
        <header class="settings-head">
          <div>
            <h2>{title}</h2>
            <p class="muted">
              {about}
              {guide && (
                <>
                  {" "}
                  <button
                    type="button"
                    class="text-link"
                    onClick={() => {
                      props.onGuide(guide);
                    }}
                  >
                    Guide
                  </button>
                </>
              )}
            </p>
          </div>
          <IconButton label="Close settings" onClick={props.onClose}>
            <X size={18} />
          </IconButton>
        </header>
        {props.section === "providers" && <ProvidersSettings onVault={vault} />}
        {props.section === "vault" && <VaultSettings focus={vaultFocus} />}
        {props.section === "agents" && <AgentSettings />}
        {props.section === "claude-code" && <ClaudeCodeSettings />}
        {props.section === "connectors" && (
          <ConnectorsSettings
            onVault={vaultAt}
            onGuide={() => {
              props.onGuide("Connectors-and-OAuth");
            }}
          />
        )}
        {props.section === "webhooks" && (
          <WebhooksSettings
            onGuide={() => {
              props.onGuide("Webhooks");
            }}
          />
        )}
        {props.section === "clis" && <ClisSettings />}
        {props.section === "memory" && (
          <>
            <MemoryVault />
            <Memory agent={null} />
          </>
        )}
        {props.section === "traces" && <TracesSettings />}
        {props.section === "voice" && <VoiceSettings />}
        {props.section === "updates" && <UpdatesSettings />}
        {props.section === "diagnostics" && <DiagnosticsSettings />}
        {props.section === "guides" && <Guides page={props.guide} onPage={props.onGuide} />}
        {props.section === "appearance" && (
          <AppearanceSettings look={props.look} onChange={props.onLook} />
        )}
        {props.section === "keyboard" && <ShortcutList />}
      </section>
    </main>
  );
}
