// Keyboard shortcuts: one table for the handler, Settings › Keyboard and the shortcut list.

import { useEffect, useRef } from "preact/hooks";

export type ShortcutId =
  | "switcher"
  | "new-agent"
  | "settings"
  | "usage"
  | "details"
  | "sidebar"
  | "agent"
  | "help"
  | "close";

export interface Shortcut {
  id: ShortcutId;
  /** `KeyboardEvent.key` in lower case; for "agent" the digits 1–9 match by position. */
  key: string;
  shift?: boolean;
  label: string;
}

export const SHORTCUTS: Shortcut[] = [
  { id: "switcher", key: "k", label: "Go to an agent or a settings page" },
  { id: "new-agent", key: "n", label: "New agent" },
  { id: "settings", key: ",", label: "Settings" },
  { id: "usage", key: "u", shift: true, label: "Usage" },
  { id: "details", key: "i", label: "Show or hide the details panel" },
  { id: "sidebar", key: "b", label: "Collapse or expand the sidebar" },
  { id: "agent", key: "1", label: "Open the 1st to 9th agent in the sidebar" },
  { id: "help", key: "/", label: "Keyboard shortcuts" },
  { id: "close", key: "escape", label: "Close the open dialog, menu, page or panel" },
];

export type KeyPress = Pick<
  KeyboardEvent,
  "key" | "code" | "metaKey" | "ctrlKey" | "shiftKey" | "altKey" | "repeat" | "isComposing"
>;

/** The shortcut a key press stands for (`n`: the agent's place). Only Esc matches without ⌘/Ctrl. */
export function matchShortcut(e: KeyPress, mac: boolean): { id: ShortcutId; n?: number } | null {
  if (e.isComposing || e.repeat || e.altKey) return null;
  if (e.key === "Escape") return { id: "close" };
  if (!(mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey)) return null;
  // By position, so AZERTY's unshifted top row works; with Shift it may be punctuation ("/").
  const digit = /^Digit([1-9])$/.exec(e.code);
  if (digit && !e.shiftKey) return { id: "agent", n: Number(digit[1]) };
  const key = e.key.toLowerCase();
  const hit = SHORTCUTS.find((s) => s.key === key && s.id !== "agent" && s.id !== "close");
  // Punctuation may need Shift (German "/" is Shift+7), so only letters check it.
  if (!hit || (/^[a-z]$/.test(key) && Boolean(hit.shift) !== e.shiftKey)) return null;
  return { id: hit.id };
}

/** How a shortcut is written: ⌘⇧U on macOS, Ctrl+Shift+U elsewhere. */
export function keyLabel(s: Shortcut, mac: boolean): string {
  if (s.id === "close") return "Esc";
  const key = s.id === "agent" ? "1–9" : s.key.toUpperCase();
  return mac ? `⌘${s.shift ? "⇧" : ""}${key}` : `Ctrl+${s.shift ? "Shift+" : ""}${key}`;
}

export function isMac(): boolean {
  return /Mac|iPhone|iPad/.test(navigator.userAgent);
}

/** Calls `on` for each shortcut pressed anywhere in the window, unless a dialog or menu is open. */
export function useShortcuts(on: (id: ShortcutId, n?: number) => void): void {
  const latest = useRef(on);
  latest.current = on;
  useEffect(() => {
    const mac = isMac();
    const onKey = (e: KeyboardEvent): void => {
      const hit = matchShortcut(e, mac);
      if (!hit) return;
      if (hit.id !== "close") e.preventDefault();
      // Dialogs, menus, popovers and pick lists handle their own keys, Esc included.
      if (document.querySelector('[role="dialog"], [role="menu"], [role="listbox"]')) return;
      latest.current(hit.id, hit.n);
    };
    // Capture, so this runs before a dialog's own Esc handler removes the dialog.
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
    };
  }, []);
}
