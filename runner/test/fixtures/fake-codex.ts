// Scripted stand-in for `codex app-server`: JSON-RPC lines on stdio. Plays
// $CODEX_HOME/scenario.json and appends what it receives to received.jsonl.
import { appendFileSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { createInterface } from "node:readline";

export type Params = Record<string, unknown>;

export type Step =
  /** A notification; threadId, turnId and turn.id are filled in unless given. */
  | { notify: string; params?: Params }
  /** A server request; the fake waits for its answer. */
  | { request: string; params?: Params }
  /** Waits for a client request with this method and answers `{}`. */
  | { await: string }
  | { exit: number };

export interface Scenario {
  /** Thread ids thread/resume accepts. */
  resumable?: string[];
  /** Steps per turn/start, counted across restarts. */
  turns: Step[][];
}

interface Msg {
  id?: string | number;
  method?: string;
  params?: Params;
}

const home = process.env["CODEX_HOME"] ?? ".";
const logFile = join(home, "received.jsonl");
const scenario = JSON.parse(readFileSync(join(home, "scenario.json"), "utf8")) as Scenario;
const record = (line: string): void => {
  appendFileSync(logFile, `${line}\n`);
};
const send = (msg: Record<string, unknown>): void => {
  process.stdout.write(`${JSON.stringify(msg)}\n`);
};

record(
  JSON.stringify({
    argv: process.argv.slice(2),
    cwd: process.cwd(),
    token: process.env["FERROBOT_AGENT_TOKEN"],
  }),
);

const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]();
async function read(): Promise<Msg | null> {
  const next = await lines.next();
  if (next.done === true) return null;
  record(next.value);
  return JSON.parse(next.value) as Msg;
}

let thread = "";
let serverIds = 0;

function fill(params: Params | undefined, turn: string): Params {
  const out: Params = { threadId: thread, turnId: turn, ...params };
  const inner = out["turn"];
  if (typeof inner === "object" && inner !== null) out["turn"] = { id: turn, ...inner };
  return out;
}

async function play(steps: Step[], turn: string): Promise<void> {
  for (const step of steps) {
    if ("notify" in step) {
      send({ method: step.notify, params: fill(step.params, turn) });
    } else if ("request" in step) {
      const id = `srv-${String(++serverIds)}`;
      send({ id, method: step.request, params: fill(step.params, turn) });
      let msg = await read();
      while (msg && msg.id !== id) msg = await read();
    } else if ("await" in step) {
      let msg = await read();
      while (msg && msg.method !== step.await) msg = await read();
      if (msg?.id !== undefined) send({ id: msg.id, result: {} });
    } else {
      process.exit(step.exit);
    }
  }
}

for (let msg = await read(); msg; msg = await read()) {
  const params = msg.params ?? {};
  switch (msg.method ?? "") {
    case "initialize":
      send({
        id: msg.id,
        result: {
          userAgent: "fake-codex",
          codexHome: home,
          platformFamily: "unix",
          platformOs: "linux",
        },
      });
      break;
    case "thread/start":
      thread = "thread-new";
      send({ id: msg.id, result: { thread: { id: thread } } });
      break;
    case "thread/resume": {
      const id = params["threadId"];
      if (typeof id === "string" && scenario.resumable?.includes(id)) {
        thread = id;
        send({ id: msg.id, result: { thread: { id } } });
      } else {
        send({ id: msg.id, error: { code: -32600, message: "no rollout found" } });
      }
      break;
    }
    case "turn/start": {
      const count = readFileSync(logFile, "utf8")
        .split("\n")
        .filter((l) => l.includes('"method":"turn/start"')).length;
      const turn = `turn-${String(count)}`;
      send({
        id: msg.id,
        result: { turn: { id: turn, status: "inProgress", items: [], error: null } },
      });
      // Like Codex; items can arrive before the driver has read the response above.
      send({
        method: "turn/started",
        params: { threadId: thread, turn: { id: turn, status: "inProgress" } },
      });
      await play(scenario.turns[count - 1] ?? [], turn);
      break;
    }
  }
}
