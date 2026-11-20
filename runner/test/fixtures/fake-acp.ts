// Scripted stand-in for an ACP agent (`gemini --acp`): JSON-RPC 2.0 lines on stdio.
// Plays $HOME/scenario.json and appends what it receives to $HOME/received.jsonl.
import { appendFileSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as sleep } from "node:timers/promises";

export type Params = Record<string, unknown>;

export type Step =
  /** A session/update notification for the current session. */
  | { update: Params }
  /** A server request; the fake waits for its answer. */
  | { request: string; params?: Params }
  /** Waits for a client message with this method. */
  | { await: string }
  /** Answers session/prompt with this stop reason (end_turn when a turn has none). */
  | { stop: string }
  /** Answers session/prompt with a JSON-RPC error. */
  | { error: string }
  | { exit: number };

export interface Scenario {
  protocolVersion?: number;
  agentCapabilities?: Params;
  authMethods?: Params[];
  /** Session ids session/load accepts. */
  loadable?: string[];
  /** Updates replayed after answering session/load, as Gemini 0.67 does. */
  replay?: Params[];
  /** Steps per session/prompt, counted across restarts. */
  turns: Step[][];
}

interface Msg {
  id?: string | number;
  method?: string;
  params?: Params;
}

const home = process.env["HOME"] ?? ".";
const logFile = join(home, "received.jsonl");
const scenario = JSON.parse(readFileSync(join(home, "scenario.json"), "utf8")) as Scenario;
const record = (line: string): void => {
  appendFileSync(logFile, `${line}\n`);
};
const send = (msg: Params): void => {
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", ...msg })}\n`);
};

let settings: unknown = null;
try {
  settings = JSON.parse(readFileSync(join(home, ".gemini", "settings.json"), "utf8"));
} catch {
  // Recorded as null: the driver did not write it before spawning.
}
record(
  JSON.stringify({
    argv: process.argv.slice(2),
    cwd: process.cwd(),
    trust: process.env["GEMINI_CLI_TRUST_WORKSPACE"],
    apiKey: process.env["GEMINI_API_KEY"],
    baseUrl: process.env["GOOGLE_GEMINI_BASE_URL"],
    settings,
  }),
);

const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]();
async function read(): Promise<Msg | null> {
  const next = await lines.next();
  if (next.done === true) return null;
  record(next.value);
  return JSON.parse(next.value) as Msg;
}

let session = "";
let serverIds = 0;
const update = (params: Params): void => {
  send({ method: "session/update", params: { sessionId: session, update: params } });
};

async function play(steps: Step[]): Promise<Params> {
  for (const step of steps) {
    if ("update" in step) {
      update(step.update);
    } else if ("request" in step) {
      const id = `srv-${String(++serverIds)}`;
      send({ id, method: step.request, params: { sessionId: session, ...step.params } });
      let msg = await read();
      while (msg && msg.id !== id) msg = await read();
    } else if ("await" in step) {
      let msg = await read();
      while (msg && msg.method !== step.await) msg = await read();
    } else if ("stop" in step) {
      return { result: { stopReason: step.stop } };
    } else if ("error" in step) {
      return { error: { code: 429, message: step.error } };
    } else {
      process.exit(step.exit);
    }
  }
  return { result: { stopReason: "end_turn" } };
}

for (let msg = await read(); msg; msg = await read()) {
  const params = msg.params ?? {};
  switch (msg.method ?? "") {
    case "initialize":
      send({
        id: msg.id,
        result: {
          protocolVersion: scenario.protocolVersion ?? 1,
          agentCapabilities: scenario.agentCapabilities ?? {
            loadSession: true,
            mcpCapabilities: { http: true, sse: true },
          },
          authMethods: scenario.authMethods ?? [
            { id: "oauth-personal", name: "Log in with Google" },
            { id: "gemini-api-key", name: "Gemini API key" },
          ],
          agentInfo: { name: "fake-acp", version: "0.0.0" },
        },
      });
      break;
    case "authenticate":
      send({ id: msg.id, result: {} });
      break;
    case "session/new":
      session = "sess-new";
      send({ id: msg.id, result: { sessionId: session } });
      break;
    case "session/load": {
      const id = params["sessionId"];
      if (typeof id === "string" && scenario.loadable?.includes(id)) {
        session = id;
        send({ id: msg.id, result: {} });
        for (const replayed of scenario.replay ?? []) {
          await sleep(20);
          update(replayed);
        }
      } else {
        send({ id: msg.id, error: { code: -32002, message: "Resource not found" } });
      }
      break;
    }
    case "session/prompt": {
      const count = readFileSync(logFile, "utf8")
        .split("\n")
        .filter((l) => l.includes('"method":"session/prompt"')).length;
      send({ id: msg.id, ...(await play(scenario.turns[count - 1] ?? [])) });
      break;
    }
    default:
      if (msg.id !== undefined)
        send({ id: msg.id, error: { code: -32601, message: `no ${String(msg.method)}` } });
  }
}
