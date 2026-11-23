// An ACP (Agent Client Protocol) agent in the sandbox; for now Gemini CLI (`gemini --acp`).
// Its model traffic goes through the daemon's proxy: GEMINI_API_KEY is this agent's token.
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as sleep } from "node:timers/promises";

import type { ToolGate } from "../gate.ts";
import { errorMessage, log } from "../log.ts";
import { decide, offGroups, type BuiltinGroup, type Decision } from "../permissions.ts";
import type { TurnEvent } from "../protocol.ts";
import { aborted, clip, systemAppend, toolResultText, turnPrompt } from "./claude.ts";
import type { ModelDriver, TurnContext, TurnResult } from "./types.ts";
import { TurnAbortedError } from "./types.ts";

const ACP_VERSION = 1;
const GEMINI_AUTH = "gemini-api-key";
const CANCEL_TIMEOUT_MS = 10_000;
// shortcut: Gemini 0.67 replays a loaded session after answering session/load, so we wait for the
// replay to go quiet; drop this once Gemini replays before it answers, as the spec says.
const REPLAY_QUIET_MS = 200;
const REPLAY_MAX_MS = 5_000;
const STDERR_TAIL = 2000;

type Params = Record<string, unknown>;

interface RpcMessage {
  id?: string | number;
  method?: string;
  params?: Params;
  result?: unknown;
  error?: { code?: number; message?: string };
}

interface InitializeResult {
  protocolVersion?: unknown;
  agentCapabilities?: { loadSession?: unknown; mcpCapabilities?: { http?: unknown } };
  authMethods?: { id?: unknown }[];
}

interface ToolCallUpdate {
  toolCallId: string;
  name?: string | null;
  title?: string | null;
  kind?: string | null;
  status?: string | null;
  content?: { type: string; content?: unknown; path?: string; newText?: string }[] | null;
  locations?: { path: string }[] | null;
  rawInput?: unknown;
}

interface SessionUpdate {
  sessionUpdate: string;
  messageId?: string | null;
  content?: unknown;
}

interface PermissionOption {
  optionId: string;
  kind: string;
}

export interface AcpDriverOptions {
  bin: string;
  model: string;
  mcpUrl: URL;
  token: string;
  workspaceDir: string;
  /** $HOME of the agent; Gemini reads `~/.gemini/settings.json` there. */
  homeDir: string;
  gate?: ToolGate;
}

/** Gemini CLI's built-in tools per group (checked against Gemini CLI 0.67's tool list). */
const GEMINI_TOOLS: Readonly<Record<BuiltinGroup, readonly string[]>> = {
  shell: ["run_shell_command", "list_background_processes", "read_background_output"],
  files: [
    "read_file",
    "read_many_files",
    "write_file",
    "replace",
    "list_directory",
    "glob",
    "grep_search",
  ],
  web: ["web_fetch", "google_web_search"],
};
/** Sub-agents run their own tools, so they go whenever a built-in group is off. */
const GEMINI_SUBAGENT = "invoke_agent";
const MANAGED = new Set([...Object.values(GEMINI_TOOLS).flat(), GEMINI_SUBAGENT]);

/** Selects Gemini's API-key auth in `~/.gemini/settings.json` and excludes the tools of `off`
 * groups (Gemini then does not offer them), keeping every other setting. */
export async function writeGeminiSettings(
  homeDir: string,
  off: ReadonlySet<BuiltinGroup> = new Set(),
): Promise<void> {
  const file = join(homeDir, ".gemini", "settings.json");
  let raw = "";
  let settings: Params = {};
  try {
    raw = await readFile(file, "utf8");
    settings = object(JSON.parse(raw));
  } catch (err) {
    // An unparsable file may be valid JSONC; rewriting it would lose the operator's settings.
    if ((err as NodeJS.ErrnoException).code !== "ENOENT") {
      throw new Error(`cannot update ${file}: ${errorMessage(err)}`, { cause: err });
    }
  }
  const security = object(settings["security"]);
  const auth = object(security["auth"]);
  settings["security"] = { ...security, auth: { ...auth, selectedType: GEMINI_AUTH } };
  const tools = object(settings["tools"]);
  const own = Array.isArray(tools["exclude"]) ? (tools["exclude"] as unknown[]) : [];
  const exclude = [
    ...own.filter((n) => typeof n !== "string" || !MANAGED.has(n)),
    ...[...off].flatMap((g) => GEMINI_TOOLS[g]),
    ...(off.size > 0 ? [GEMINI_SUBAGENT] : []),
  ];
  if (exclude.length > 0 || "tools" in settings) settings["tools"] = { ...tools, exclude };
  const text = `${JSON.stringify(settings, null, 2)}\n`;
  if (text === raw) return;
  await mkdir(dirname(file), { recursive: true });
  await writeFile(file, text);
}

function object(value: unknown): Params {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Params)
    : {};
}

function commandOf(call: ToolCallUpdate): string {
  const command = object(call.rawInput)["command"];
  // Gemini sends no rawInput; its shell tool's title is the command.
  return typeof command === "string" ? command : (call.title ?? "");
}

function pathsOf(call: ToolCallUpdate): string[] {
  return [
    ...(call.locations ?? []).map((l) => l.path),
    ...(call.content ?? []).flatMap((c) =>
      c.type === "diff" && typeof c.path === "string" ? [c.path] : [],
    ),
  ];
}

export function toolUse(call: ToolCallUpdate): { name: string; input: unknown } {
  if (call.kind === "execute") return { name: "Bash", input: { command: commandOf(call) } };
  // Gemini sends no name; its MCP call ids start with `mcp_ferrobot_<tool>__`.
  const mcp = /^mcp_ferrobot_(\w+?)__/.exec(call.toolCallId);
  const name = call.name ?? (mcp ? `mcp__ferrobot__${mcp[1] ?? ""}` : (call.kind ?? "tool"));
  return { name, input: call.rawInput ?? { title: call.title } };
}

function contentText(call: ToolCallUpdate): string {
  return (call.content ?? [])
    .map((c) => {
      if (c.type === "content") return toolResultText([c.content]);
      if (c.type === "diff") return `${c.path ?? ""}\n${c.newText ?? ""}`;
      return `[${c.type}]`;
    })
    .join("\n");
}

function review(
  call: ToolCallUpdate,
  home: string,
  off: ReadonlySet<BuiltinGroup>,
): { name: string; verdict: Decision; command?: string } {
  const { name } = toolUse(call);
  const paths = (tool: string): Decision => {
    const files = pathsOf(call);
    if (files.length === 0) return { allow: false, reason: "blocked: file access without a path" };
    return (
      files.map((file_path) => decide(tool, { file_path }, home, off)).find((v) => !v.allow) ?? {
        allow: true,
      }
    );
  };
  switch (call.kind ?? "other") {
    case "execute": {
      const command = commandOf(call);
      return { name, verdict: decide("Bash", { command }, home, off), command };
    }
    case "fetch":
      return { name, verdict: decide("WebFetch", {}, home, off) };
    case "edit":
    case "delete":
    case "move":
      return { name, verdict: paths("Write") };
    case "read":
    case "search":
      return { name, verdict: paths("Read") };
    default:
      return { name, verdict: { allow: true } };
  }
}

/** The agent process: JSON-RPC 2.0, one message per line on stdio. */
class Connection {
  readonly #child: ChildProcessWithoutNullStreams;
  readonly #pending = new Map<
    string | number,
    { resolve: (result: unknown) => void; reject: (err: Error) => void }
  >();
  #nextId = 0;
  #stderr = "";
  #exit: Error | null = null;
  /** When the last session/update arrived (epoch ms). */
  lastUpdate = 0;
  onUpdate: (params: Params) => void = () => {};
  /** The result to send back, or undefined for requests the runner does not handle. */
  onRequest: (method: string, params: Params) => unknown = () => undefined;

  static async start(
    bin: string,
    args: string[],
    cwd: string,
    env: NodeJS.ProcessEnv,
  ): Promise<Connection> {
    const child = spawn(bin, args, { cwd, env, stdio: "pipe" });
    await new Promise<void>((resolve, reject) => {
      child.once("spawn", resolve);
      child.once("error", reject);
    });
    return new Connection(child);
  }

  constructor(child: ChildProcessWithoutNullStreams) {
    this.#child = child;
    const gone = (reason: string): void => {
      if (this.#exit) return;
      const stderr = this.#stderr.trim();
      this.#exit = new Error(stderr ? `${reason}: ${stderr}` : reason);
      for (const pending of this.#pending.values()) pending.reject(this.#exit);
      this.#pending.clear();
    };
    // EPIPE once the process is gone; the close handler reports that.
    child.stdin.on("error", () => {});
    child.on("error", (err) => {
      gone(`gemini failed: ${err.message}`);
    });
    child.on("close", (code, signal) => {
      gone(`gemini exited with ${code === null ? (signal ?? "a signal") : `code ${String(code)}`}`);
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

  notify(method: string, params: Params): void {
    this.#send({ method, params });
  }

  kill(): void {
    this.#child.kill();
  }

  /** Answers a request from the agent; the handler may wait (e.g. for the operator's card). */
  async #answer(id: string | number, method: string, params: Params): Promise<void> {
    let result: unknown;
    try {
      result = await this.onRequest(method, params);
    } catch (err) {
      log.warn("acp: could not handle a request", { method, error: errorMessage(err) });
    }
    this.#send(
      result === undefined
        ? { id, error: { code: -32601, message: `ferrobot does not handle ${method}` } }
        : { id, result },
    );
  }

  #send(message: RpcMessage): void {
    this.#child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);
  }

  #receive(line: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      parsed = null;
    }
    if (typeof parsed !== "object" || parsed === null) {
      log.warn("acp: ignoring a line that is not a JSON object", { line: line.slice(0, 200) });
      return;
    }
    const msg = parsed as RpcMessage;
    const params = msg.params ?? {};
    if (msg.id !== undefined && msg.method !== undefined) {
      void this.#answer(msg.id, msg.method, params);
    } else if (msg.method === "session/update") {
      this.lastUpdate = Date.now();
      try {
        this.onUpdate(params);
      } catch (err) {
        log.warn("acp: could not handle a session update", { error: errorMessage(err) });
      }
    } else if (msg.id !== undefined && msg.method === undefined) {
      const pending = this.#pending.get(msg.id);
      this.#pending.delete(msg.id);
      if (msg.error) pending?.reject(new Error(msg.error.message ?? "gemini request failed"));
      else pending?.resolve(msg.result);
    }
  }
}

interface Session {
  id: string;
  /** Instructions already sent in this session, if any. */
  instructions: string | null;
}

export class AcpDriver implements ModelDriver {
  readonly name = "acp";
  readonly #opts: AcpDriverOptions;
  #conn: Connection | null = null;
  #caps = { loadSession: false, mcpHttp: false };
  #session: Session | null = null;
  #stale = false;
  /** The off groups the running agent's settings were written for. */
  #off = "";

  constructor(opts: AcpDriverOptions) {
    this.#opts = opts;
  }

  async runTurn(ctx: TurnContext): Promise<TurnResult> {
    if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
    const conn = await this.#start(offGroups(ctx.tools));
    const session = await this.#openSession(conn, ctx.sessionId);
    if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
    return { result: await this.#turn(conn, session, ctx), sessionId: session.id };
  }

  close(): Promise<void> {
    this.#conn?.kill();
    this.#conn = null;
    return Promise.resolve();
  }

  reconfigure(): void {
    this.#stale = true;
  }

  async #start(off: ReadonlySet<BuiltinGroup>): Promise<Connection> {
    const key = [...off].sort().join(",");
    if (this.#conn?.alive && !this.#stale && this.#off === key) return this.#conn;
    // A fresh agent process reads its settings and lists the MCP tools anew; the session loads by id.
    this.#conn?.kill();
    this.#stale = false;
    this.#off = key;
    this.#session = null;
    const { bin, model, workspaceDir, homeDir } = this.#opts;
    // GOOGLE_GEMINI_BASE_URL alone would make Gemini pick its "gateway" auth, which headless mode rejects.
    await writeGeminiSettings(homeDir, off);
    let conn: Connection;
    try {
      conn = await Connection.start(bin, ["--acp", "--model", model], workspaceDir, {
        ...process.env,
        HOME: homeDir,
        GEMINI_CLI_TRUST_WORKSPACE: "true",
      });
    } catch (err) {
      throw new Error(
        `gemini is not installed in this sandbox image (${bin}: ${errorMessage(err)})`,
        { cause: err },
      );
    }
    try {
      const init = (await conn.request("initialize", {
        protocolVersion: ACP_VERSION,
        // No fs/terminal: the agent uses its own tools inside the sandbox.
        clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
        clientInfo: { name: "ferrobot", version: "0.1.5" },
      })) as InitializeResult | null;
      if (init?.protocolVersion !== ACP_VERSION) {
        throw new Error(
          `gemini speaks ACP version ${String(init?.protocolVersion)}, ferrobot speaks ${String(ACP_VERSION)}`,
        );
      }
      if (init.authMethods?.some((m) => m.id === GEMINI_AUTH)) {
        await conn.request("authenticate", { methodId: GEMINI_AUTH });
      }
      const caps = init.agentCapabilities;
      this.#caps = {
        loadSession: caps?.loadSession === true,
        mcpHttp: caps?.mcpCapabilities?.http === true,
      };
      if (!this.#caps.mcpHttp)
        log.warn("gemini cannot reach HTTP MCP servers; the ferrobot tools are unavailable");
    } catch (err) {
      conn.kill();
      throw err;
    }
    this.#conn = conn;
    return conn;
  }

  async #openSession(conn: Connection, wanted: string | null): Promise<Session> {
    if (wanted && this.#session?.id === wanted) return this.#session;
    const { mcpUrl, token, workspaceDir } = this.#opts;
    const common = {
      cwd: workspaceDir,
      mcpServers: this.#caps.mcpHttp
        ? [
            {
              type: "http",
              name: "ferrobot",
              url: mcpUrl.href,
              headers: [{ name: "Authorization", value: `Bearer ${token}` }],
            },
          ]
        : [],
    };
    let id: string | null = null;
    if (wanted && this.#caps.loadSession) {
      try {
        await conn.request("session/load", { ...common, sessionId: wanted });
        await replayDone(conn);
        id = wanted;
      } catch (err) {
        // A session that cannot be loaded (e.g. after restoring an older snapshot) must not wedge the agent.
        if (!conn.alive) throw err;
        log.warn("could not load session; starting a new one", {
          session: wanted,
          error: errorMessage(err),
        });
      }
    } else if (wanted) {
      log.info("gemini cannot load sessions; starting a new one", { session: wanted });
    }
    if (id === null) {
      const sessionId = (
        (await conn.request("session/new", common)) as { sessionId?: unknown } | null
      )?.sessionId;
      if (typeof sessionId !== "string") throw new Error("gemini returned no session id");
      id = sessionId;
    }
    this.#session = { id, instructions: null };
    return this.#session;
  }

  async #turn(conn: Connection, session: Session, ctx: TurnContext): Promise<string | null> {
    const { emit, signal } = ctx;
    const home = this.#opts.workspaceDir;
    // Gemini has no system-prompt channel over ACP, so the instructions lead the prompt instead.
    const instructions = systemAppend(ctx.agentName, ctx.instructions);
    const text =
      session.instructions === instructions
        ? turnPrompt(ctx)
        : `${instructions}\n\n${turnPrompt(ctx)}`;
    session.instructions = instructions;

    const calls = new Map<string, ToolCallUpdate>();
    const finished = new Set<string>();
    let message = "";
    let messageId: string | null = null;
    let last: string | null = null;
    const flush = (): void => {
      if (message.trim()) {
        emit({ kind: "assistant_text", text: message });
        last = message;
      }
      message = "";
    };
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

    conn.onUpdate = (params) => {
      if (params["sessionId"] !== session.id) return;
      const update = params["update"] as SessionUpdate;
      if (update.sessionUpdate === "agent_message_chunk") {
        const content = object(update.content);
        if (content["type"] !== "text" || typeof content["text"] !== "string" || !content["text"])
          return;
        if (update.messageId && update.messageId !== messageId) {
          flush();
          messageId = update.messageId;
        }
        message += content["text"];
        emit({ kind: "assistant_delta", text: content["text"] });
      } else if (
        update.sessionUpdate === "tool_call" ||
        update.sessionUpdate === "tool_call_update"
      ) {
        flush();
        for (const event of toolEvents(update as unknown as ToolCallUpdate, calls, finished))
          emit(event);
      }
    };
    conn.onRequest = async (method, params) => {
      if (method !== "session/request_permission" || params["sessionId"] !== session.id)
        return undefined;
      const asked = params["toolCall"] as ToolCallUpdate;
      const reviewed = review(
        { ...calls.get(asked.toolCallId), ...asked },
        home,
        offGroups(ctx.tools),
      );
      const { name } = reviewed;
      let verdict = reviewed.verdict;
      if (verdict.allow && reviewed.command !== undefined && this.#opts.gate)
        verdict = await this.#opts.gate.shell(reviewed.command);
      const options = (params["options"] ?? []) as PermissionOption[];
      const pick = (kind: string): string | undefined =>
        options.find((o) => o.kind === kind)?.optionId;
      // Never allow_always: the agent would remember it and stop asking decide().
      const optionId = verdict.allow
        ? pick("allow_once")
        : (pick("reject_once") ?? pick("reject_always"));
      if (!verdict.allow) emit({ kind: "tool_denied", name, reason: verdict.reason });
      return {
        outcome:
          optionId === undefined ? { outcome: "cancelled" } : { outcome: "selected", optionId },
      };
    };

    try {
      const prompt = conn.request("session/prompt", {
        sessionId: session.id,
        prompt: [{ type: "text", text }],
      });
      const end = await Promise.race([prompt, stopped]);
      if (end === "aborted") {
        conn.notify("session/cancel", { sessionId: session.id });
        const settled = await Promise.race([
          Promise.allSettled([prompt]),
          sleep(CANCEL_TIMEOUT_MS, "timeout", { ref: false }),
        ]);
        // An agent that ignores the cancel is wedged; the next turn starts a fresh one.
        if (settled === "timeout") conn.kill();
        throw new TurnAbortedError("interrupted");
      }
      flush();
      const stopReason = (end as { stopReason?: unknown } | null)?.stopReason;
      if (stopReason === "end_turn") return last;
      throw new Error(`gemini ended the turn with ${String(stopReason)}`);
    } finally {
      conn.onUpdate = () => {};
      conn.onRequest = () => undefined;
    }
  }
}

function toolEvents(
  update: ToolCallUpdate,
  calls: Map<string, ToolCallUpdate>,
  finished: Set<string>,
): TurnEvent[] {
  const id = update.toolCallId;
  const known = calls.get(id);
  const call = { ...known, ...update };
  calls.set(id, call);
  const events: TurnEvent[] = [];
  if (!known) events.push({ kind: "tool_use", id, ...toolUse(call) });
  if ((call.status === "completed" || call.status === "failed") && !finished.has(id)) {
    finished.add(id);
    events.push({
      kind: "tool_result",
      id,
      output: clip(contentText(update) || call.status),
      is_error: call.status === "failed",
    });
  }
  return events;
}

async function replayDone(conn: Connection): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < REPLAY_MAX_MS) {
    const idle = Date.now() - Math.max(conn.lastUpdate, start);
    if (idle >= REPLAY_QUIET_MS) return;
    await sleep(REPLAY_QUIET_MS - idle);
  }
}
