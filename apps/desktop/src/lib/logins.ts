import type { LoginSpec, LoginView } from "../api.ts";

export interface LoginPreset {
  id: string;
  label: string;
  /** Where to create the token. */
  keyUrl: string;
  spec: Omit<LoginSpec, "agents">;
}

/** Logins that need nothing but a token and agents. `gh` and `git` have their own risky rules. */
export const LOGIN_PRESETS: LoginPreset[] = [
  {
    id: "github",
    label: "GitHub",
    keyUrl: "https://github.com/settings/personal-access-tokens/new",
    spec: {
      hosts: ["github.com", "api.github.com", "uploads.github.com"],
      env: ["GH_TOKEN", "GITHUB_TOKEN"],
      programs: ["gh"],
      confirm: false,
    },
  },
];

export const presetOf = (id: string): LoginPreset | undefined =>
  LOGIN_PRESETS.find((p) => p.id === id);

/** A login's name in the app: the preset's label, else its id. */
export const loginLabel = (id: string): string => presetOf(id)?.label ?? id;

/** Words from a field that takes several, split by commas, spaces or new lines. */
export const splitList = (text: string): string[] => [
  ...new Set(
    text
      .split(/[\s,]+/)
      .map((w) => w.trim())
      .filter(Boolean),
  ),
];

/** The login as `PUT /v1/logins/{id}` takes it. */
export const specOf = (l: LoginView): LoginSpec => ({
  hosts: l.hosts,
  env: l.env,
  programs: l.programs ?? [],
  confirm: l.confirm ?? false,
  agents: l.agents ?? [],
});
