import type { Agent, ModelInfo, Provider } from "../api.ts";

/** The model an agent runs: its own, else its provider's default. */
export function modelLabel(
  agent: Pick<Agent, "model" | "provider_id">,
  provider: Pick<Provider, "default_model"> | undefined,
): string {
  return (
    agent.model ?? provider?.default_model ?? (agent.provider_id ? "provider default" : "default")
  );
}

/** `[id, label]` options for a model picker. A current id the list lacks stays selectable; no list means no options. */
export function modelOptions(models: readonly ModelInfo[], current: string): [string, string][] {
  return withSaved(
    models.map((m) => [m.id, m.name && m.name !== m.id ? `${m.name} (${m.id})` : m.id]),
    current,
  );
}

/** `options` with a saved id the list lacks kept selectable; no options stay none. */
export function withSaved(options: [string, string][], saved: string): [string, string][] {
  return options.length > 0 && saved && !options.some(([v]) => v === saved)
    ? [[saved, saved], ...options]
    : options;
}
