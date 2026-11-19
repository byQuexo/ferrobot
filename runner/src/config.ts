import { homedir } from "node:os";
import { join } from "node:path";

export type DriverKind = "mock" | "claude";

export interface RunnerConfig {
  /** Agent API base, e.g. http://host.docker.internal:7879 */
  daemonUrl: URL;
  token: string;
  agentName: string;
  driver: DriverKind;
  model: string | undefined;
  /** Where the agent works (its "desktop"). */
  workspaceDir: string;
  /** Runner bookkeeping (session id, audit log). */
  stateDir: string;
}

export class ConfigError extends Error {
  override name = "ConfigError";
}

function required(env: NodeJS.ProcessEnv, key: string): string {
  const value = env[key]?.trim();
  if (!value) throw new ConfigError(`missing required environment variable ${key}`);
  return value;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): RunnerConfig {
  const rawUrl = required(env, "FERROBOT_DAEMON_URL");
  let daemonUrl: URL;
  try {
    daemonUrl = new URL(rawUrl);
  } catch {
    throw new ConfigError(`FERROBOT_DAEMON_URL is not a valid URL: ${rawUrl}`);
  }
  if (daemonUrl.protocol !== "http:" && daemonUrl.protocol !== "https:") {
    throw new ConfigError("FERROBOT_DAEMON_URL must be http(s)");
  }
  const driver = env["FERROBOT_DRIVER"] ?? "mock";
  if (driver !== "mock" && driver !== "claude") {
    throw new ConfigError(`FERROBOT_DRIVER must be "mock" or "claude", got ${driver}`);
  }
  if (driver === "claude" && !env["ANTHROPIC_API_KEY"]) {
    throw new ConfigError("FERROBOT_DRIVER=claude needs ANTHROPIC_API_KEY");
  }
  const home = env["HOME"] ?? homedir();
  return {
    daemonUrl,
    token: required(env, "FERROBOT_AGENT_TOKEN"),
    agentName: env["FERROBOT_AGENT_NAME"] ?? "agent",
    driver,
    model: env["FERROBOT_MODEL"] || undefined,
    workspaceDir: env["FERROBOT_WORKSPACE_DIR"] ?? join(home, "workspace"),
    stateDir: env["FERROBOT_STATE_DIR"] ?? join(home, ".ferrobot"),
  };
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
