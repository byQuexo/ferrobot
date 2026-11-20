// Codex (`codex app-server`) in the sandbox; its model traffic goes through the daemon's proxy.
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as sleep } from "node:timers/promises";

import { errorMessage, log } from "../log.ts";
import { decide, type Decision } from "../permissions.ts";
import type { TurnEvent } from "../protocol.ts";
import { aborted, clip, systemAppend, toolResultText, turnPrompt } from "./claude.ts";
import type { ModelDriver, TurnContext, TurnResult } from "./types.ts";
import { TurnAbortedError } from "./types.ts";

const PROVIDER = "ferrobot";
const TOKEN_ENV = "FERROBOT_AGENT_TOKEN";
// create_agent waits for the operator's decision (FERROBOT_APPROVAL_TIMEOUT, 300 s by default).
const MCP_TOOL_TIMEOUT_SEC = 600;
const INTERRUPT_TIMEOUT_MS = 10_000;
const STDERR_TAIL = 2000;

type Params = Record<string, unknown>;

interface RpcMessage {
  id?: string | number;
  method?: string;
  params?: Params;
  result?: unknown;
  error?: { code?: number; message?: string };
}

interface FileUpdate {
  path: string;
  kind: { type: string; move_path?: string | null };
  diff: string;
}

type CodexItem =
  | { type: "agentMessage"; id: string; text: string }
  | {
      type: "commandExecution";
      id: string;
      command: string;
      status: string;
      aggregatedOutput: string | null;
    }
  | { type: "fileChange"; id: string; status: string; changes: FileUpdate[] }
  | {
      type: "mcpToolCall";
      id: string;
      server: string;
      tool: string;
      status: string;
      arguments: unknown;
      result: { content: unknown } | null;
      error: { message: string } | null;
    };

export interface CodexDriverOptions {
  bin: string;
  /** Responses API base of the daemon's model proxy, e.g. http://host.docker.internal:7879/model */
  baseUrl: URL;
  mcpUrl: URL;
  model: string;
  token: string;
  workspaceDir: string;
  stateDir: string;
}

/** `$CODEX_HOME/config.toml`. Names the token's env var; never holds a secret. */
export function codexConfigToml(opts: { model: string; baseUrl: URL; mcpUrl: URL }): string {
  // A JSON string literal is also a valid TOML basic string.
  const str = (value: string): string => JSON.stringify(value);
  return [
    `model = ${str(opts.model)}`,
    `model_provider = ${str(PROVIDER)}`,
    // Codex >= 0.166 rejects "untrusted"; requests it still sends are answered with decide().
    `approval_policy = "on-request"`,
    // The container is the boundary; Codex's own Landlock/Seatbelt may not work inside it.
    `sandbox_mode = "danger-full-access"`,
    "",
    `[model_providers.${PROVIDER}]`,
    `name = ${str(PROVIDER)}`,
    `base_url = ${str(opts.baseUrl.href.replace(/\/+$/, ""))}`,
    `env_key = ${str(TOKEN_ENV)}`,
    `wire_api = "responses"`,
    "",
    `[mcp_servers.${PROVIDER}]`,
    `url = ${str(opts.mcpUrl.href)}`,
    `bearer_token_env_var = ${str(TOKEN_ENV)}`,
    // decide() allows every ferrobot tool; Codex would otherwise ask for unannotated tools.
    `default_tools_approval_mode = "approve"`,
    `tool_timeout_sec = ${MCP_TOOL_TIMEOUT_SEC}`,
    "",
  ].join("\n");
}

function toolResult(id: string, status: string, output: string): TurnEvent {
  return { kind: "tool_result", id, output: clip(output), is_error: status !== "completed" };
}

function itemEvents(stage: "started" | "completed", item: CodexItem): TurnEvent[] {
  const { id } = item;
  switch (item.type) {
    case "agentMessage":
      return stage === "completed" && item.text.trim()
        ? [{ kind: "assistant_text", text: item.text }]
        : [];
    case "commandExecution":
      return stage === "started"
        ? [{ kind: "tool_use", id, name: "Bash", input: { command: item.command } }]
        : [toolResult(id, item.status, item.aggregatedOutput ?? item.status)];
    case "fileChange":
      return stage === "started"
        ? [
            {
              kind: "tool_use",
              id,
              name: "apply_patch",
              input: { changes: item.changes.map((c) => ({ path: c.path, kind: c.kind.type })) },
            },
          ]
        : [
            toolResult(
              id,
              item.status,
              item.changes.map((c) => `${c.kind.type} ${c.path}\n${c.diff}`).join("\n"),
            ),
          ];
    case "mcpToolCall":
      return stage === "started"
        ? [
            {
              kind: "tool_use",
              id,
              name: `mcp__${item.server}__${item.tool}`,
              input: item.arguments,
            },
          ]
        : [
            toolResult(
              id,
              item.status,
              item.error?.message ?? toolResultText(item.result?.content),
            ),
          ];
    default:
      return [];
  }
}

function changedPaths(changes: FileUpdate[]): string[] {
  return changes.flatMap((c) => (c.kind.move_path ? [c.path, c.kind.move_path] : [c.path]));
}

function review(
  method: string,
  params: Params,
  filesByItem: ReadonlyMap<string, string[]>,
  home: string,
): { name: string; verdict: Decision } | null {
  switch (method) {
    case "item/commandExecution/requestApproval": {
      const command = typeof params["command"] === "string" ? params["command"] : "";
      return { name: "Bash", verdict: decide("Bash", { command }, home) };
    }
    case "item/fileChange/requestApproval": {
      // The request names only the item; its paths arrived with item/started.
      const itemId = params["itemId"];
      const files = typeof itemId === "string" ? filesByItem.get(itemId) : undefined;
      if (!files)
        return {
          name: "apply_patch",
          verdict: { allow: false, reason: "blocked: unknown file change" },
        };
      const denied = files
        .map((file_path) => decide("Write", { file_path }, home))
        .find((v) => !v.allow);
      return { name: "apply_patch", verdict: denied ?? { allow: true } };
    }
    default:
      return null;
  }
}

function turnOf(params: Params): unknown {
  return params["turnId"] ?? (params["turn"] as { id?: unknown } | undefined)?.id;
}

function threadIdOf(result: unknown): string {
  const id = (result as { thread?: { id?: unknown } } | null)?.thread?.id;
  if (typeof id !== "string") throw new Error("codex returned no thread id");
  return id;
}

/** One `codex app-server` process: JSON-RPC without the "jsonrpc" field, one message per line. */
class AppServer {
  readonly #child: ChildProcessWithoutNullStreams;
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

  static async start(bin: string, cwd: string, env: NodeJS.ProcessEnv): Promise<AppServer> {
    const child = spawn(bin, ["app-server"], { cwd, env, stdio: "pipe" });
    await new Promise<void>((resolve, reject) => {
      child.once("spawn", resolve);
      child.once("error", reject);
    });
    return new AppServer(child);
  }

  constructor(child: ChildProcessWithoutNullStreams) {
    this.#child = child;
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
      gone(`codex app-server failed: ${err.message}`);
    });
    child.on("close", (code, signal) => {
      gone(
        `codex app-server exited with ${code === null ? (signal ?? "a signal") : `code ${String(code)}`}`,
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

  notify(method: string): void {
    this.#send({ method });
  }

  kill(): void {
    this.#child.kill();
  }

  #send(message: RpcMessage): void {
    this.#child.stdin.write(`${JSON.stringify(message)}\n`);
  }

  #receive(line: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      parsed = null;
    }
    if (typeof parsed !== "object" || parsed === null) {
      log.warn("codex: ignoring a line that is not a JSON object", { line: line.slice(0, 200) });
      return;
    }
    const msg = parsed as RpcMessage;
    const params = msg.params ?? {};
    if (msg.id !== undefined && msg.method !== undefined) {
      let result: unknown;
      try {
        result = this.onRequest(msg.method, params);
      } catch (err) {
        log.warn("codex: could not handle a request", {
          method: msg.method,
          error: errorMessage(err),
        });
      }
      // Codex waits for every answer, so an unhandled request gets an error, never silence.
      this.#send(
        result === undefined
          ? {
              id: msg.id,
              error: { code: -32601, message: `ferrobot does not handle ${msg.method}` },
            }
          : { id: msg.id, result },
      );
    } else if (msg.method !== undefined) {
      try {
        this.onNotification(msg.method, params);
      } catch (err) {
        log.warn("codex: could not handle a notification", {
          method: msg.method,
          error: errorMessage(err),
        });
      }
    } else if (msg.id !== undefined) {
      const pending = this.#pending.get(msg.id);
      this.#pending.delete(msg.id);
      if (msg.error) pending?.reject(new Error(msg.error.message ?? "codex request failed"));
      else pending?.resolve(msg.result);
    }
  }
}

export class CodexDriver implements ModelDriver {
  readonly name = "codex";
  readonly #opts: CodexDriverOptions;
  #server: AppServer | null = null;
  #thread: { id: string; instructions: string } | null = null;
  #stale = false;

  constructor(opts: CodexDriverOptions) {
    this.#opts = opts;
  }

  async runTurn(ctx: TurnContext): Promise<TurnResult> {
    if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
    const server = await this.#start();
    const threadId = await this.#openThread(server, ctx);
    if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
    return { result: await this.#turn(server, threadId, ctx), sessionId: threadId };
  }

  close(): Promise<void> {
    this.#server?.kill();
    this.#server = null;
    return Promise.resolve();
  }

  reconfigure(): void {
    this.#stale = true;
  }

  async #start(): Promise<AppServer> {
    if (this.#server?.alive && !this.#stale) return this.#server;
    // A fresh app-server lists the MCP tools anew; the thread resumes by id.
    this.#server?.kill();
    this.#stale = false;
    this.#thread = null;
    const home = join(this.#opts.stateDir, "codex");
    await mkdir(home, { recursive: true, mode: 0o700 });
    await writeFile(join(home, "config.toml"), codexConfigToml(this.#opts), { mode: 0o600 });
    let server: AppServer;
    try {
      server = await AppServer.start(this.#opts.bin, this.#opts.workspaceDir, {
        ...process.env,
        CODEX_HOME: home,
        [TOKEN_ENV]: this.#opts.token,
      });
    } catch (err) {
      throw new Error(
        `codex is not installed in this sandbox image (${this.#opts.bin}: ${errorMessage(err)})`,
        {
          cause: err,
        },
      );
    }
    try {
      await server.request("initialize", { clientInfo: { name: "ferrobot", version: "0.1.0" } });
      server.notify("initialized");
    } catch (err) {
      server.kill();
      throw err;
    }
    this.#server = server;
    return server;
  }

  async #openThread(server: AppServer, ctx: TurnContext): Promise<string> {
    const instructions = systemAppend(ctx.agentName, ctx.instructions);
    const wanted = ctx.sessionId;
    if (wanted && this.#thread?.id === wanted && this.#thread.instructions === instructions)
      return wanted;
    const common = { cwd: this.#opts.workspaceDir, developerInstructions: instructions };
    let id: string | null = null;
    if (wanted) {
      try {
        id = threadIdOf(
          await server.request("thread/resume", {
            ...common,
            threadId: wanted,
            excludeTurns: true,
          }),
        );
      } catch (err) {
        // A thread that cannot be resumed (e.g. after restoring an older snapshot) must not wedge the agent.
        if (!server.alive) throw err;
        log.warn("could not resume session; starting a new one", {
          session: wanted,
          error: errorMessage(err),
        });
      }
    }
    id ??= threadIdOf(await server.request("thread/start", common));
    this.#thread = { id, instructions };
    return id;
  }

  async #turn(server: AppServer, threadId: string, ctx: TurnContext): Promise<string | null> {
    const { emit, signal } = ctx;
    const home = this.#opts.workspaceDir;
    const filesByItem = new Map<string, string[]>();
    let turnId: unknown = null;
    let lastText: string | null = null;
    let lastError: string | null = null;
    let finish: (turn: { status: string; error: { message: string } | null }) => void = () => {};
    const finished = new Promise<Parameters<typeof finish>[0]>((resolve) => {
      finish = resolve;
    });
    const stopped = new Promise<"aborted">((resolve) => {
      if (aborted(signal)) resolve("aborted");
      signal.addEventListener(
        "abort",
        () => {
          resolve("aborted");
        },
        { once: true },
      );
    });

    // Anything of another turn (e.g. the tail of an interrupted one) is dropped.
    const ours = (params: Params): boolean =>
      params["threadId"] === threadId && turnId !== null && turnOf(params) === turnId;
    server.onNotification = (method, params) => {
      if (method === "turn/started" && turnId === null && params["threadId"] === threadId)
        turnId = turnOf(params);
      if (!ours(params)) return;
      switch (method) {
        case "item/agentMessage/delta":
          if (typeof params["delta"] === "string" && params["delta"]) {
            emit({ kind: "assistant_delta", text: params["delta"] });
          }
          break;
        case "item/started":
        case "item/completed": {
          const item = params["item"] as CodexItem;
          const stage = method === "item/started" ? "started" : "completed";
          if (stage === "started" && item.type === "fileChange")
            filesByItem.set(item.id, changedPaths(item.changes));
          if (stage === "completed" && item.type === "agentMessage" && item.text.trim())
            lastText = item.text;
          for (const event of itemEvents(stage, item)) emit(event);
          break;
        }
        case "error": {
          const message = (params["error"] as { message?: unknown } | undefined)?.message;
          if (params["willRetry"] !== true && typeof message === "string") lastError = message;
          break;
        }
        case "turn/completed":
          finish(params["turn"] as Parameters<typeof finish>[0]);
          break;
      }
    };
    server.onRequest = (method, params) => {
      if (!ours(params)) return undefined;
      const reviewed = review(method, params, filesByItem, home);
      if (!reviewed) return undefined;
      if (reviewed.verdict.allow) return { decision: "accept" };
      emit({ kind: "tool_denied", name: reviewed.name, reason: reviewed.verdict.reason });
      return { decision: "decline" };
    };

    try {
      const started = await server.request("turn/start", {
        threadId,
        input: [{ type: "text", text: turnPrompt(ctx) }],
      });
      turnId ??= turnOf((started ?? {}) as Params);
      const end = await Promise.race([finished, server.closed, stopped]);
      if (end === "aborted") {
        await interrupt(server, { threadId, turnId });
        throw new TurnAbortedError("interrupted");
      }
      if (end instanceof Error) throw end;
      if (end.status === "completed") return lastText;
      if (aborted(signal)) throw new TurnAbortedError("interrupted");
      throw new Error(end.error?.message ?? lastError ?? `codex turn ended as ${end.status}`);
    } finally {
      server.onNotification = () => {};
      server.onRequest = () => undefined;
    }
  }
}

async function interrupt(server: AppServer, params: Params): Promise<void> {
  const answer = await Promise.race([
    Promise.allSettled([server.request("turn/interrupt", params)]),
    sleep(INTERRUPT_TIMEOUT_MS, "timeout", { ref: false }),
  ]);
  // A Codex that ignores the interrupt is wedged; the next turn starts a fresh one.
  if (answer === "timeout") server.kill();
}
