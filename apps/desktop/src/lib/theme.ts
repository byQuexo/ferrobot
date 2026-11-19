export type ThemePref = "system" | "light" | "dark";

const KEY = "ferrobot.theme";

export function loadTheme(): ThemePref {
  const v = localStorage.getItem(KEY);
  return v === "light" || v === "dark" ? v : "system";
}

/** Applies the preference to <html data-theme> and remembers it. */
export function applyTheme(pref: ThemePref): void {
  localStorage.setItem(KEY, pref);
  const dark = pref === "dark" || (pref === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset["theme"] = dark ? "dark" : "light";
}

/** Re-applies "system" when the OS theme changes. Returns an unsubscribe function. */
export function followSystem(getPref: () => ThemePref): () => void {
  const mq = matchMedia("(prefers-color-scheme: dark)");
  const onChange = (): void => {
    if (getPref() === "system") applyTheme("system");
  };
  mq.addEventListener("change", onChange);
  return () => { mq.removeEventListener("change", onChange); };
}
