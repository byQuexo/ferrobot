import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";

import type { MemoryVaultView } from "../api.ts";
import { useApp } from "../context.ts";
import { PrefRow } from "./AppearanceSettings.tsx";
import { Button } from "./ui.tsx";

/** Keeps memories as Markdown notes in an Obsidian vault instead of the built-in memory. */
export function MemoryVault(): JSX.Element | null {
  const { client, run } = useApp();
  const [view, setView] = useState<MemoryVaultView | null>(null);
  const [path, setPath] = useState("");

  useEffect(() => {
    void run(() => client.memoryVault()).then((v) => {
      setView(v ?? null);
    });
  }, []);

  if (!view) return null;
  const use = async (next: string | null): Promise<void> => {
    const saved = await run(() => client.setMemoryVault(next));
    if (saved) {
      setView(saved);
      setPath("");
    }
  };
  const copied =
    view.copied && view.path
      ? ` Copied ${String(view.copied)} ${view.copied === 1 ? "memory" : "memories"} into it.`
      : "";
  const hint = view.path
    ? `Memories are notes in ${view.path}/ferrobot: one folder per agent and “Global memory”.${copied}${
        view.obsidian ? "" : " This folder has no .obsidian folder; the notes are plain Markdown."
      }`
    : "Keep memories as Markdown notes in your vault, so you read and edit them in Obsidian. Off: the built-in memory.";

  return (
    <ul class="pref-list memory-vault">
      <PrefRow title="Obsidian vault" hint={hint}>
        {view.path ? (
          <Button small onClick={() => void use(null)}>
            Stop using it
          </Button>
        ) : (
          <span class="choice-custom">
            <input
              aria-label="Vault folder"
              value={path}
              spellcheck={false}
              autoComplete="off"
              placeholder="~/Obsidian/Notes"
              onInput={(e) => {
                setPath(e.currentTarget.value);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && path.trim()) void use(path);
              }}
            />
            <Button small disabled={!path.trim()} onClick={() => void use(path)}>
              Use
            </Button>
          </span>
        )}
      </PrefRow>
    </ul>
  );
}
