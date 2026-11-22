import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import {
  AudioLines,
  BookOpen,
  Bot,
  Brain,
  Cpu,
  KeyRound,
  Monitor,
  Moon,
  Palette,
  Plug,
  RefreshCw,
  Sun,
  Terminal,
  X,
} from "lucide-preact";

import type { GeneralSettings } from "../api.ts";
import { useApp } from "../context.ts";
import type { ThemePref } from "../lib/theme.ts";
import { ClisSettings } from "./ClisSettings.tsx";
import { ConnectorsSettings } from "./ConnectorsSettings.tsx";
import { Guides } from "./Guides.tsx";
import { Memory } from "./InfoPanel.tsx";
import { ProvidersSettings } from "./ProvidersSettings.tsx";
import { UpdatesSettings } from "./UpdatesSettings.tsx";
import { VaultSettings } from "./VaultSettings.tsx";
import { VoiceSettings } from "./VoiceSettings.tsx";
import { IconButton, Switch } from "./ui.tsx";

export type SettingsSection =
  | "providers"
  | "vault"
  | "agents"
  | "connectors"
  | "clis"
  | "memory"
  | "voice"
  | "appearance"
  | "updates"
  | "guides";

/** Id, label, about, icon and the guide page behind the header's "Guide" link. */
const SECTIONS: [SettingsSection, string, string, typeof KeyRound, string?][] = [
  [
    "providers",
    "Model providers",
    "The models your agents run on. Their keys live in Vault.",
    Cpu,
    "Providers",
  ],
  [
    "vault",
    "Vault",
    "Every key in one place: model providers, connectors and voice. Kept in the keychain of the daemon’s computer; agents never see them.",
    KeyRound,
  ],
  ["agents", "Agents", "What agents may do without asking you.", Bot, "Tools-and-Permissions"],
  [
    "connectors",
    "Connectors",
    "Remote MCP servers whose tools you can give to agents, with a token or an OAuth sign-in.",
    Plug,
    "Connectors-and-OAuth",
  ],
  [
    "clis",
    "CLIs",
    "Command-line tools in every agent’s computer, built into one shared image.",
    Terminal,
    "CLIs",
  ],
  [
    "memory",
    "Global memory",
    "Facts every agent can recall. Agents add to it; you can edit and remove entries.",
    Brain,
  ],
  [
    "voice",
    "Voice",
    "Agents’ replies read aloud and spoken messages. Start a call with the phone button in a chat.",
    AudioLines,
    "Voice-and-Calls",
  ],
  ["appearance", "Appearance", "How ferrobot looks on this computer.", Palette],
  ["updates", "Updates", "The version ferrobot runs and how to get the latest.", RefreshCw],
  ["guides", "Guides", "How ferrobot works, step by step. The same pages as the wiki.", BookOpen],
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
          Agents create teammates without asking first. The agent limit, blocked commands and keys
          stay protected.
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

/** All app-wide settings on one page. */
export function Settings(props: {
  section: SettingsSection;
  onSection: (s: SettingsSection) => void;
  onClose: () => void;
  theme: ThemePref;
  onTheme: (t: ThemePref) => void;
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
  const vault = (): void => {
    props.onSection("vault");
  };
  return (
    <main class="settings" aria-label="Settings">
      <nav class="settings-nav" aria-label="Settings sections">
        <h1>Settings</h1>
        {SECTIONS.map(([id, label, , Icon]) => (
          <button
            key={id}
            type="button"
            class={`settings-tab${props.section === id ? " active" : ""}`}
            aria-current={props.section === id ? "page" : undefined}
            onClick={() => {
              props.onSection(id);
            }}
          >
            <Icon size={16} /> {label}
          </button>
        ))}
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
        {props.section === "vault" && <VaultSettings />}
        {props.section === "agents" && <AgentSettings />}
        {props.section === "connectors" && <ConnectorsSettings onVault={vault} />}
        {props.section === "clis" && <ClisSettings />}
        {props.section === "memory" && <Memory agent={null} />}
        {props.section === "voice" && <VoiceSettings onVault={vault} />}
        {props.section === "updates" && <UpdatesSettings />}
        {props.section === "guides" && <Guides page={props.guide} onPage={props.onGuide} />}
        {props.section === "appearance" && (
          <div class="form">
            <span class="field">
              Theme
              <div class="segmented" role="radiogroup" aria-label="Theme">
                {(
                  [
                    ["system", Monitor, "System"],
                    ["light", Sun, "Light"],
                    ["dark", Moon, "Dark"],
                  ] as const
                ).map(([value, Icon, label]) => (
                  <button
                    key={value}
                    type="button"
                    role="radio"
                    aria-checked={props.theme === value}
                    onClick={() => {
                      props.onTheme(value);
                    }}
                  >
                    <Icon size={16} /> {label}
                  </button>
                ))}
              </div>
            </span>
          </div>
        )}
      </section>
    </main>
  );
}
