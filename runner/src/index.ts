// Entry point of the in-sandbox agent runner.
import { mkdir } from "node:fs/promises";

import { ConfigError, loadConfig, mcpUrl, runnerWsUrl, type RunnerConfig } from "./config.ts";
import { ClaudeDriver } from "./drivers/claude.ts";
import { MockDriver } from "./drivers/mock.ts";
import { OpenAIDriver } from "./drivers/openai.ts";
import type { ModelDriver } from "./drivers/types.ts";
import { DaemonLink } from "./link.ts";
import { errorMessage, log } from "./log.ts";
import { FerrobotMcp } from "./mcp-client.ts";
import { PROTOCOL_VERSION } from "./protocol.ts";
import { Runner } from "./runner.ts";
import { SessionStore } from "./session.ts";

export const RUNNER_VERSION = "0.1.0";

async function main(): Promise<void> {
  const config = loadConfig();
  await mkdir(config.workspaceDir, { recursive: true });
  const session = new SessionStore(config.stateDir);
  const saved = await session.load();

  const driver = createDriver(config, session);

  // `link` and `runner` reference each other; the closures run after both exist.
  // eslint-disable-next-line prefer-const
  let runner: Runner;
  const link = new DaemonLink(runnerWsUrl(config.daemonUrl), config.token, {
    hello: () => ({
      type: "hello",
      protocol_version: PROTOCOL_VERSION,
      runner_version: RUNNER_VERSION,
      driver: driver.name,
      session_id: runner.sessionId,
    }),
    onFrame: (frame) => { runner.handle(frame); },
    onDisconnect: () => { runner.abortCurrent(); },
  });
  runner = new Runner(
    { driver, session, agentName: config.agentName, send: (f) => link.send(f) },
    saved.sessionId,
  );

  log.info("runner starting", { agent: config.agentName, driver: driver.name });
  link.start();

  const shutdown = (signal: string): void => {
    log.info("shutting down", { signal });
    runner.abortCurrent();
    link.close();
    void driver.close().finally(() => process.exit(0));
  };
  process.once("SIGTERM", () => { shutdown("SIGTERM"); });
  process.once("SIGINT", () => { shutdown("SIGINT"); });
}

function createDriver(config: RunnerConfig, session: SessionStore): ModelDriver {
  switch (config.driver) {
    case "claude":
      return new ClaudeDriver({
        mcpUrl: mcpUrl(config.daemonUrl),
        token: config.token,
        workspaceDir: config.workspaceDir,
        model: config.model,
        session,
      });
    case "openai":
      return new OpenAIDriver({
        baseUrl: config.openaiBaseUrl,
        model: config.model,
        token: config.token,
        workspaceDir: config.workspaceDir,
        stateDir: config.stateDir,
        mcp: new FerrobotMcp(mcpUrl(config.daemonUrl), config.token),
      });
    case "mock":
      return new MockDriver(new FerrobotMcp(mcpUrl(config.daemonUrl), config.token));
  }
}

main().catch((err: unknown) => {
  log.error(err instanceof ConfigError ? "invalid configuration" : "fatal", {
    error: errorMessage(err),
  });
  process.exit(1);
});
