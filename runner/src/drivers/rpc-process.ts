// JSON-RPC with an agent process, one message per line on stdio: `codex app-server` and ACP agents.
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createInterface } from "node:readline";
import { setTimeout as sleep } from "node:timers/promises";

import { errorMessage, log } from "../log.ts";

const STDERR_TAIL = 2000;
const SETTLE_TIMEOUT_MS = 10_000;

export type Params = Record<string, unknown>;

interface RpcMessage {
  id?: string | number;
  method?: string;
  params?: Params;
  result?: unknown;
  error?: { code?: number; message?: string };
}

export interface RpcNames {
  /** The agent in errors, e.g. "codex". */
  name: string;
  /** The process in exit errors, e.g. "codex app-server". */
  process: string;
  /** Prefix of log lines. */
  log: string;
  /** Sends the "jsonrpc" field; `codex app-server` goes without it. */
  jsonrpc: boolean;
}

/** Starts `bin`; one that cannot start is reported as not installed. */
export async function spawnAgent(
  name: string,
  bin: string,
  args: string[],
  cwd: string,
  env: NodeJS.ProcessEnv,
): Promise<ChildProcessWithoutNullStreams> {
  try {
    const child = spawn(bin, args, { cwd, env, stdio: "pipe" });
    await new Promise<void>((resolve, reject) => {
      child.once("spawn", resolve);
      child.once("error", reject);
    });
    return child;
  } catch (err) {
    throw new Error(
      `${name} is not installed in this sandbox image (${bin}: ${errorMessage(err)})`,
      { cause: err },
    );
  }
}

export class RpcProcess {
  readonly #child: ChildProcessWithoutNullStreams;
  readonly #names: RpcNames;
  readonly #pending = new Map<
    string | number,
    { resolve: (result: unknown) => void; reject: (err: Error) => void }
  >();
  #nextId = 0;
  #stderr = "";
  #exit: Error | null = null;
  readonly closed: Promise<Error>;
  onNotification: (method: string, params: Params) => void = () => {};
  /** The result to send back, or undefined for requests the runner does not handle. */
  onRequest: (method: string, params: Params) => unknown = () => undefined;

  constructor(child: ChildProcessWithoutNullStreams, names: RpcNames) {
    this.#child = child;
    this.#names = names;
    let close: (err: Error) => void = () => {};
    this.closed = new Promise((resolve) => {
      close = resolve;
    });
    const gone = (reason: string): void => {
      if (this.#exit) return;
      const stderr = this.#stderr.trim();
      this.#exit = new Error(stderr ? `${reason}: ${stderr}` : reason);
      for (const pending of this.#pending.values()) pending.reject(this.#exit);
      this.#pending.clear();
      close(this.#exit);
    };
    // EPIPE once the process is gone; the close handler reports that.
    child.stdin.on("error", () => {});
    child.on("error", (err) => {
      gone(`${names.process} failed: ${err.message}`);
    });
    child.on("close", (code, signal) => {
      gone(
        `${names.process} exited with ${code === null ? (signal ?? "a signal") : `code ${String(code)}`}`,
      );
    });
    child.stderr.setEncoding("utf8").on("data", (text: string) => {
      process.stderr.write(text);
      this.#stderr = (this.#stderr + text).slice(-STDERR_TAIL);
    });
    createInterface({ input: child.stdout }).on("line", (line) => {
      this.#receive(line);
    });
  }

  get alive(): boolean {
    return this.#exit === null;
  }

  request(method: string, params: Params): Promise<unknown> {
    if (this.#exit) return Promise.reject(this.#exit);
    const id = ++this.#nextId;
    return new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.#send({ id, method, params });
    });
  }

  notify(method: string, params?: Params): void {
    this.#send({ method, params });
  }

  kill(): void {
    this.#child.kill();
  }

  /** Waits for `answer`; an agent that does not give it is wedged, so the next turn starts a fresh one. */
  async settleOrKill(answer: Promise<unknown>): Promise<void> {
    const settled = await Promise.race([
      Promise.allSettled([answer]),
      sleep(SETTLE_TIMEOUT_MS, "timeout", { ref: false }),
    ]);
    if (settled === "timeout") this.kill();
  }

  /** Answers a request from the agent; the handler may wait (e.g. for the operator's card). */
  async #answer(id: string | number, method: string, params: Params): Promise<void> {
    let result: unknown;
    try {
      result = await this.onRequest(method, params);
    } catch (err) {
      log.warn(`${this.#names.log}: could not handle a request`, {
        method,
        error: errorMessage(err),
      });
    }
    // The agent waits for every answer, so an unhandled request gets an error, never silence.
    this.#send(
      result === undefined
        ? { id, error: { code: -32601, message: `ferrobot does not handle ${method}` } }
        : { id, result },
    );
  }

  #send(message: RpcMessage): void {
    const line = this.#names.jsonrpc ? { jsonrpc: "2.0", ...message } : message;
    this.#child.stdin.write(`${JSON.stringify(line)}\n`);
  }

  #receive(line: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      parsed = null;
    }
    if (typeof parsed !== "object" || parsed === null) {
      log.warn(`${this.#names.log}: ignoring a line that is not a JSON object`, {
        line: line.slice(0, 200),
      });
      return;
    }
    const msg = parsed as RpcMessage;
    const params = msg.params ?? {};
    if (msg.id !== undefined && msg.method !== undefined) {
      void this.#answer(msg.id, msg.method, params);
    } else if (msg.method !== undefined) {
      try {
        this.onNotification(msg.method, params);
      } catch (err) {
        log.warn(`${this.#names.log}: could not handle a notification`, {
          method: msg.method,
          error: errorMessage(err),
        });
      }
    } else if (msg.id !== undefined) {
      const pending = this.#pending.get(msg.id);
      this.#pending.delete(msg.id);
      if (msg.error)
        pending?.reject(new Error(msg.error.message ?? `${this.#names.name} request failed`));
      else pending?.resolve(msg.result);
    }
  }
}
