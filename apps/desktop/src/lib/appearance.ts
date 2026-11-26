// How the app looks, saved per device and applied to <html> before the first render.

/** Accent colours as [accent, accent-strong], each readable with the theme's --accent-text. */
export const ACCENTS = {
  blue: { label: "Blue", light: ["#4b63d8", "#3b51c2"], dark: ["#8197f2", "#9aabf6"] },
  violet: { label: "Violet", light: ["#7a4fd0", "#6940bc"], dark: ["#a98bf5", "#bba3f8"] },
  pink: { label: "Pink", light: ["#c2367a", "#a92c69"], dark: ["#f07fb2", "#f59bc4"] },
  red: { label: "Red", light: ["#c93d35", "#b0312a"], dark: ["#f2796f", "#f69189"] },
  orange: { label: "Orange", light: ["#b8560f", "#9e480b"], dark: ["#f0a15c", "#f4b47c"] },
  green: { label: "Green", light: ["#1e7f4f", "#186b42"], dark: ["#4cc28a", "#6dd0a1"] },
  teal: { label: "Teal", light: ["#0d7781", "#0a646d"], dark: ["#4cc1cc", "#70cfd8"] },
  graphite: { label: "Graphite", light: ["#4f5663", "#3e4450"], dark: ["#a7afbc", "#bdc3cd"] },
} as const;

export type Accent = keyof typeof ACCENTS;
export type ThemePref = "light" | "dark" | "system";

export interface Appearance {
  theme: ThemePref;
  accent: Accent;
  bubbles: "solid" | "soft" | "plain";
  chatBg: "default" | "tinted" | "plain";
  pattern: "none" | "dots" | "grid" | "lines" | "aurora";
  textSize: "small" | "default" | "large";
  font: "default" | "system" | "rounded" | "serif" | "mono";
  corners: "sharp" | "rounded" | "round";
  density: "compact" | "cozy" | "spacious";
  width: "narrow" | "default" | "wide";
  motion: "full" | "reduced";
}

export const DEFAULT_APPEARANCE: Appearance = {
  theme: "system",
  accent: "blue",
  bubbles: "soft",
  chatBg: "default",
  pattern: "none",
  textSize: "default",
  font: "default",
  corners: "rounded",
  density: "cozy",
  width: "default",
  motion: "full",
};

/** Each preference's choices with their labels, in the order Settings shows them. */
export const CHOICES: { [K in keyof Appearance]: readonly (readonly [Appearance[K], string])[] } = {
  theme: [
    ["light", "Light"],
    ["dark", "Dark"],
    ["system", "System"],
  ],
  accent: (Object.keys(ACCENTS) as Accent[]).map((id) => [id, ACCENTS[id].label] as const),
  bubbles: [
    ["solid", "Solid"],
    ["soft", "Soft"],
    ["plain", "Plain"],
  ],
  chatBg: [
    ["default", "Default"],
    ["tinted", "Tinted"],
    ["plain", "Plain"],
  ],
  pattern: [
    ["none", "None"],
    ["dots", "Dots"],
    ["grid", "Grid"],
    ["lines", "Lines"],
    ["aurora", "Aurora"],
  ],
  textSize: [
    ["small", "Small"],
    ["default", "Default"],
    ["large", "Large"],
  ],
  font: [
    ["default", "Default"],
    ["system", "System"],
    ["rounded", "Rounded"],
    ["serif", "Serif"],
    ["mono", "Mono"],
  ],
  corners: [
    ["sharp", "Sharp"],
    ["rounded", "Rounded"],
    ["round", "Round"],
  ],
  density: [
    ["compact", "Compact"],
    ["cozy", "Cozy"],
    ["spacious", "Spacious"],
  ],
  width: [
    ["narrow", "Narrow"],
    ["default", "Default"],
    ["wide", "Wide"],
  ],
  motion: [
    ["full", "Full"],
    ["reduced", "Reduced"],
  ],
};

const KEYS: Record<keyof Appearance, string> = {
  theme: "ferrobot.theme",
  accent: "ferrobot.accent",
  bubbles: "ferrobot.bubbles",
  chatBg: "ferrobot.chat-bg",
  pattern: "ferrobot.pattern",
  textSize: "ferrobot.text-size",
  font: "ferrobot.font",
  corners: "ferrobot.corners",
  density: "ferrobot.density",
  width: "ferrobot.width",
  motion: "ferrobot.motion",
};

/** The stored preferences; a missing or unknown value falls back to its default. */
export function readAppearance(get: (key: string) => string | null): Appearance {
  const pick = <K extends keyof Appearance>(k: K): Appearance[K] => {
    const stored = get(KEYS[k]);
    return CHOICES[k].find(([v]) => v === stored)?.[0] ?? DEFAULT_APPEARANCE[k];
  };
  const look: Partial<Record<keyof Appearance, string>> = {};
  for (const k of Object.keys(KEYS) as (keyof Appearance)[]) look[k] = pick(k);
  return look as Appearance;
}

export function loadAppearance(): Appearance {
  try {
    return readAppearance((key) => localStorage.getItem(key));
  } catch {
    return DEFAULT_APPEARANCE;
  }
}

export function saveAppearance(look: Appearance): void {
  try {
    for (const k of Object.keys(KEYS) as (keyof Appearance)[])
      localStorage.setItem(KEYS[k], look[k]);
  } catch {
    // Storage can be unavailable; the look then lasts until the app closes.
  }
}

/** Sets the data attributes on <html> and the accent colours styles.css reads. */
export function applyAppearance(look: Appearance): void {
  const root = document.documentElement;
  const dark =
    look.theme === "dark" ||
    (look.theme === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
  Object.assign(root.dataset, { ...look, theme: dark ? "dark" : "light" });
  const { light, dark: night } = ACCENTS[look.accent];
  root.style.setProperty("--accent-l", light[0]);
  root.style.setProperty("--accent-strong-l", light[1]);
  root.style.setProperty("--accent-d", night[0]);
  root.style.setProperty("--accent-strong-d", night[1]);
}

/** Re-applies a "system" theme when the OS theme changes. Returns an unsubscribe function. */
export function followSystem(getLook: () => Appearance): () => void {
  const mq = matchMedia("(prefers-color-scheme: dark)");
  const onChange = (): void => {
    if (getLook().theme === "system") applyAppearance(getLook());
  };
  mq.addEventListener("change", onChange);
  return () => {
    mq.removeEventListener("change", onChange);
  };
}
