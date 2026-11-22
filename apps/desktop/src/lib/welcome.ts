import type { Provider, ProviderKind } from "../api.ts";

/** The daemon's agent name rule (`AgentName` in crates/core). */
export const AGENT_NAME = /^[a-z][a-z0-9-]{0,31}$/;

/** First-run steps: add a provider, add its key, then meet the starter agent. */
export type WelcomeStep = "provider" | "key" | "meet";

// The daemon's `ProviderKind::needs_key` (crates/core/src/provider.rs).
const KEYLESS: readonly ProviderKind[] = ["vertex", "ollama", "openai_compatible", "codex"];

/** Whether agents can reach a model through `p` now. */
export const providerReady = (p: Provider): boolean => p.has_key || KEYLESS.includes(p.kind);

/** The welcome step to show instead of the empty main view; `null` when there is none. */
export function welcomeStep(s: {
  agents: number;
  /** `null` while not loaded. */
  providers: readonly Provider[] | null;
  /** The daemon's own `ANTHROPIC_API_KEY`. */
  daemonKey: boolean;
  skipped: boolean;
}): WelcomeStep | null {
  if (s.agents > 0 || s.skipped || s.providers === null) return null;
  if (s.daemonKey || s.providers.some(providerReady)) return "meet";
  return s.providers.length ? "key" : "provider";
}

/** The starter agent's name: the first entry when the naming convention is a list of names, else `guide`. */
export function starterName(convention: string | null | undefined): string {
  const names = (convention ?? "").split(",").map((n) => n.trim());
  // shortcut: a convention in prose ("Greek gods") needs a model to follow, so it gets `guide`.
  return names.length > 1 && names.every((n) => AGENT_NAME.test(n))
    ? (names[0] ?? "guide")
    : "guide";
}

export const STARTER_DESCRIPTION = "Helps you set up ferrobot";

export const STARTER_INSTRUCTIONS = `You are the operator's first agent in ferrobot: a friendly setup helper. Keep answers short and plain.

ferrobot runs a team of AI agents. Each agent has its own computer (a sandbox with a shell and files), a mailbox to message the operator and other agents, a memory, and routines that run on a schedule.

You can create agents and routines with your tools. The operator approves each one unless Auto mode is on. Remember what you learn about the operator.

The operator sets these up in the app; you cannot change settings, so point to Settings → Guides for the steps:
- model providers (Settings → Model providers; their keys go in Vault)
- connectors to services like Linear or Notion (Settings → Connectors)
- command-line tools in every agent's computer (Settings → CLIs)
- voice: dictation and calls (Settings → Voice)
- what agents may do without asking (Settings → Agents)

Never ask for API keys or tokens in chat. They belong in Vault.`;

export const STARTER_HELLO =
  "Hi! I just set you up. Introduce yourself in two sentences, then ask what you can do for me.";
