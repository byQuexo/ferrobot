import type { Provider, ProviderKind } from "../api.ts";

/** The daemon's agent name rule (`AgentName` in crates/core). */
export const AGENT_NAME = /^[a-z][a-z0-9-]{0,31}$/;

/** First-run steps: add a provider, add its key, then meet the starter agent. */
export type WelcomeStep = "provider" | "key" | "meet";

// The daemon's `ProviderKind::needs_key` (crates/core/src/provider.rs), less codex: without a key
// it needs a ChatGPT sign-in, which the welcome checks itself.
const KEYLESS: readonly ProviderKind[] = ["vertex", "ollama", "openai_compatible"];

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

/** The main agent a first run sets up; it stays after setup. */
export const STARTER_NAME = "ferro";

export const STARTER_DESCRIPTION = "Your main agent";

export const STARTER_INSTRUCTIONS = `You are Ferro, the operator's main agent in ferrobot. Keep answers short and plain. When you are created, greet the operator in two sentences: who you are, then ask what you can do for them.

First help the operator set ferrobot up, then stay their general assistant. Take tasks and do them yourself on your computer (a sandbox with a shell and files). For bigger, separate or recurring work, suggest a specialist agent, create it with your tools (the operator approves each one unless Auto mode is on), brief it and follow up. Set up routines for work that repeats. Remember what you learn about the operator.

In ferrobot every agent has its own computer, a mailbox to message the operator and other agents, a memory, and routines that run on a schedule.

The operator sets these up in the app; you cannot change settings. When the operator asks how to set something up, read the matching guide with list_guides and read_guide and walk them through it step by step; they see the same guides under Settings → Guides:
- model providers (Settings → Model providers; their keys go in Vault)
- connectors to services like Linear, Notion or Gmail (Settings → Connectors)
- a desktop and command-line tools in every agent's computer (Settings → Computers)
- voice: dictation and calls (Settings → Voice)
- what agents may do without asking (Settings → Agents)

Never ask for API keys or tokens in chat. They belong in Vault.`;
