import { homedir } from "node:os";
import { join } from "node:path";

export type DriverKind = "mock" | "claude" | "openai" | "codex" | "acp";

interface CommonConfig {
  /** Agent API base, e.g. http://host.docker.internal:7879 */
  daemonUrl: URL;
  token: string;
  agentName: string;
  model: string | undefined;
  /** Where the agent works (its "desktop"). */
  workspaceDir: string;
  /** Runner bookkeeping (session id, audit log). */
  stateDir: string;
}

export type RunnerConfig = CommonConfig &
  (
    | { driver: "mock" | "claude" }
    // openaiBaseUrl: the daemon's OpenAI-compatible model proxy, e.g. http://host.docker.internal:7879/model
    | { driver: "openai"; model: string; openaiBaseUrl: URL }
    // codexBaseUrl: the proxy's Responses API base; Codex POSTs <base>/responses
    | { driver: "codex"; model: string; codexBaseUrl: URL; codexBin: string }
    // acpBin: the ACP agent (Gemini CLI); GEMINI_API_KEY and GOOGLE_GEMINI_BASE_URL reach it through the env
    | { driver: "acp"; model: string; acpBin: string }
  );

export class ConfigError extends Error {
  override name = "ConfigError";
}

function required(env: NodeJS.ProcessEnv, key: string): string {
  const value = env[key]?.trim();
  if (!value) throw new ConfigError(`missing required environment variable ${key}`);
  return value;
}

function httpUrl(env: NodeJS.ProcessEnv, key: string): URL {
  const raw = required(env, key);
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    throw new ConfigError(`${key} is not a valid URL: ${raw}`);
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new ConfigError(`${key} must be http(s)`);
  }
  return url;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): RunnerConfig {
  const daemonUrl = httpUrl(env, "FERROBOT_DAEMON_URL");
  const driver = env["FERROBOT_DRIVER"] ?? "mock";
  if (driver !== "mock" && driver !== "claude" && driver !== "openai" && driver !== "codex" && driver !== "acp") {
    throw new ConfigError(`FERROBOT_DRIVER must be "mock", "claude", "openai", "codex" or "acp", got ${driver}`);
  }
  if (driver === "claude" && !env["ANTHROPIC_API_KEY"]) {
    throw new ConfigError("FERROBOT_DRIVER=claude needs ANTHROPIC_API_KEY");
  }
  const home = env["HOME"] ?? homedir();
  const common: CommonConfig = {
    daemonUrl,
    token: required(env, "FERROBOT_AGENT_TOKEN"),
    agentName: env["FERROBOT_AGENT_NAME"] ?? "agent",
    model: env["FERROBOT_MODEL"] || undefined,
    workspaceDir: env["FERROBOT_WORKSPACE_DIR"] ?? join(home, "workspace"),
    stateDir: env["FERROBOT_STATE_DIR"] ?? join(home, ".ferrobot"),
  };
  if (driver === "openai") {
    return {
      ...common,
      driver,
      model: required(env, "FERROBOT_MODEL"),
      openaiBaseUrl: httpUrl(env, "FERROBOT_OPENAI_BASE_URL"),
    };
  }
  if (driver === "codex") {
    return {
      ...common,
      driver,
      model: required(env, "FERROBOT_MODEL"),
      codexBaseUrl: httpUrl(env, "FERROBOT_CODEX_BASE_URL"),
      codexBin: env["FERROBOT_CODEX_BIN"]?.trim() || "codex",
    };
  }
  if (driver === "acp") {
    const agent = required(env, "FERROBOT_ACP_AGENT");
    if (agent !== "gemini") throw new ConfigError(`FERROBOT_ACP_AGENT must be "gemini", got ${agent}`);
    // Without the proxy URL Gemini would send the agent token to Google itself.
    httpUrl(env, "GOOGLE_GEMINI_BASE_URL");
    required(env, "GEMINI_API_KEY");
    return {
      ...common,
      driver,
      model: required(env, "FERROBOT_MODEL"),
      acpBin: env["FERROBOT_ACP_BIN"]?.trim() || "gemini",
    };
  }
  return { ...common, driver };
}

/** WebSocket URL of the runner endpoint. */
export function runnerWsUrl(base: URL): URL {
  const url = new URL("/runner/ws", base);
  url.protocol = base.protocol === "https:" ? "wss:" : "ws:";
  return url;
}

/** URL of the daemon's MCP endpoint. */
export function mcpUrl(base: URL): URL {
  return new URL("/mcp", base);
}
