import type { ModelInfo } from "../api.ts";

/** `[id, label]` options for a model picker. A current id the list lacks stays selectable; no list means no options. */
export function modelOptions(models: readonly ModelInfo[], current: string): [string, string][] {
  if (models.length === 0) return [];
  const options = models.map((m): [string, string] => [
    m.id,
    m.name && m.name !== m.id ? `${m.name} (${m.id})` : m.id,
  ]);
  return current && !models.some((m) => m.id === current)
    ? [[current, current], ...options]
    : options;
}

/** `options` with a saved id the list lacks kept selectable, like `modelOptions` does for models. */
export function withSaved(options: [string, string][], saved: string): [string, string][] {
  return options.length > 0 && saved && !options.some(([v]) => v === saved)
    ? [[saved, saved], ...options]
    : options;
}
