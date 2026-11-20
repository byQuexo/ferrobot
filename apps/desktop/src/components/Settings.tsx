import type { JSX } from "preact";
import { Brain, KeyRound, Monitor, Moon, Palette, Plug, Sun, X } from "lucide-preact";

import type { ThemePref } from "../lib/theme.ts";
import { ConnectorsSettings } from "./ConnectorsSettings.tsx";
import { Memory } from "./InfoPanel.tsx";
import { ProvidersSettings } from "./ProvidersSettings.tsx";
import { IconButton } from "./ui.tsx";

export type SettingsSection = "providers" | "connectors" | "memory" | "appearance";

const SECTIONS: [SettingsSection, string, string, typeof KeyRound][] = [
  ["providers", "Model providers", "The models your agents run on. Keys stay in the keychain of the daemon’s computer.", KeyRound],
  ["connectors", "Connectors", "Remote MCP servers whose tools you can give to agents, with a token or an OAuth sign-in.", Plug],
  ["memory", "Global memory", "Facts every agent can recall. Agents add to it; you can edit and remove entries.", Brain],
  ["appearance", "Appearance", "How ferrobot looks on this computer.", Palette],
];

/** All app-wide settings on one page. */
export function Settings(props: {
  section: SettingsSection;
  onSection: (s: SettingsSection) => void;
  onClose: () => void;
  theme: ThemePref;
  onTheme: (t: ThemePref) => void;
}): JSX.Element {
  const [, title, about] = SECTIONS.find(([id]) => id === props.section) ?? ["providers", "Settings", "", KeyRound];
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
            onClick={() => { props.onSection(id); }}
          >
            <Icon size={16} /> {label}
          </button>
        ))}
      </nav>
      <section class="settings-body">
        <header class="settings-head">
          <div>
            <h2>{title}</h2>
            <p class="muted">{about}</p>
          </div>
          <IconButton label="Close settings" onClick={props.onClose}>
            <X size={18} />
          </IconButton>
        </header>
        {props.section === "providers" && <ProvidersSettings />}
        {props.section === "connectors" && <ConnectorsSettings />}
        {props.section === "memory" && <Memory agent={null} />}
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
                    onClick={() => { props.onTheme(value); }}
                  >
                    <Icon size={14} /> {label}
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
