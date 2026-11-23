// Any OpenAI-compatible Chat Completions API; it has no agent loop, so the driver runs one. Requests go
// to the daemon's model proxy with this agent's token; the real key and the upstream stay in the daemon.
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";

import type { ToolGate } from "../gate.ts";
import { errorMessage, log } from "../log.ts";
import type { FerrobotMcp } from "../mcp-client.ts";
import { decide, offGroups, type BuiltinGroup, type Decision } from "../permissions.ts";
import { aborted, clip, latePrompt, systemAppend, turnPrompt } from "./claude.ts";
import { ThinkFilter } from "./think.ts";
import type { ModelDriver, TurnContext, TurnResult } from "./types.ts";
import { TurnAbortedError } from "./types.ts";

const MCP_PREFIX = "mcp__ferrobot__";
const MAX_MODEL_CALLS = 50;
const BASH_TIMEOUT_MS = 120_000;
/** The model sees more of a tool's output than the timeline does. */
const MAX_MODEL_TOOL_OUTPUT = 20_000;
/** Per stream; keeps a runaway command from exhausting the runner's memory. */
const MAX_CAPTURE = 1_000_000;
// create_agent waits for the operator's decision (FERROBOT_APPROVAL_TIMEOUT, 300 s by default).
const MCP_CALL_TIMEOUT_MS = 600_000;
const SESSION_ID = /^[\w-]+$/;

interface ToolCall {
  id: string;
  type: "function";
  function: { name: string; arguments: string };
}

export type ChatMessage =
  | { role: "system" | "user"; content: string }
  | { role: "assistant"; content: string; tool_calls?: ToolCall[] }
  | { role: "tool"; tool_call_id: string; content: string };

interface StreamChunk {
  error?: unknown;
  choices?: {
    delta?: {
      content?: string | null;
      tool_calls?: {
        index?: number;
        id?: string;
        function?: { name?: string; arguments?: string };
      }[];
    };
  }[];
}

interface RequestedCall {
  id: string;
  name: string;
  args: string;
  /** Null when `args` is not a JSON object. */
  input: Record<string, unknown> | null;
}

interface ToolOutcome {
  output: string;
  isError: boolean;
}

export interface OpenAIDriverOptions {
  /** OpenAI-compatible base, e.g. http://host.docker.internal:7879/model */
  baseUrl: URL;
  model: string;
  token: string;
  workspaceDir: string;
  stateDir: string;
  mcp: FerrobotMcp;
  fetch?: typeof fetch;
  gate?: ToolGate;
}

function functionTool(name: string, description: string, properties: Record<string, unknown>) {
  return {
    type: "function" as const,
    function: {
      name,
      description,
      parameters: { type: "object", properties, required: Object.keys(properties) },
    },
  };
}

const BUILTIN_TOOLS: { group: BuiltinGroup; tool: ReturnType<typeof functionTool> }[] = [
  {
    group: "shell",
    tool: functionTool(
      "bash",
      "Run a shell command (bash -lc) in your workspace. Times out after 120 s.",
      {
        command: { type: "string" },
        description: {
          type: "string",
          description:
            'What this does in a few plain words for your operator, e.g. "Checking flight prices".',
        },
      },
    ),
  },
  {
    group: "files",
    tool: functionTool("read_file", "Read a text file. Relative paths are inside your workspace.", {
      path: { type: "string" },
    }),
  },
  {
    group: "files",
    tool: functionTool(
      "write_file",
      "Write a text file, creating parent directories. Relative paths are inside your workspace.",
      {
        path: { type: "string" },
        content: { type: "string" },
      },
    ),
  },
];

function toolsLine(names: string[]): string {
  if (names.length === 0) return "";
  const list =
    names.length > 1
      ? `${names.slice(0, -1).join(", ")} and ${names.at(-1) ?? ""}`
      : names.join("");
  return `On your own computer you can use the ${list} tool${names.length > 1 ? "s" : ""}.\n`;
}

/** Inlines `$defs` refs and drops `$schema`: several upstreams (Gemini among them) reject those keywords. */
export function plainSchema(schema: object): Record<string, unknown> {
  const root = schema as { $defs?: Record<string, unknown> };
  const defs = root.$defs ?? {};
  const walk = (node: unknown, depth: number): unknown => {
    if (Array.isArray(node)) return node.map((n) => walk(n, depth));
    if (typeof node !== "object" || node === null) return node;
    const ref = (node as { $ref?: unknown }).$ref;
    if (typeof ref === "string" && ref.startsWith("#/$defs/") && depth < 16) {
      return walk(defs[ref.slice("#/$defs/".length)] ?? {}, depth + 1);
    }
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(node)) {
      if (k !== "$schema" && k !== "$defs") out[k] = walk(v, depth);
    }
    return out;
  };
  return walk(schema, 0) as Record<string, unknown>;
}

export class OpenAIDriver implements ModelDriver {
  readonly name = "openai";
  readonly steering = true;
  readonly #opts: OpenAIDriverOptions;
  readonly #fetch: typeof fetch;

  constructor(opts: OpenAIDriverOptions) {
    this.#opts = opts;
    this.#fetch = opts.fetch ?? fetch;
  }

  async runTurn(ctx: TurnContext): Promise<TurnResult> {
    if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
    try {
      return await this.#turn(ctx);
    } catch (err) {
      if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
      throw err;
    }
  }

  async close(): Promise<void> {
    await this.#opts.mcp.close();
  }

  async #turn(ctx: TurnContext): Promise<TurnResult> {
    const { id, history } = await this.#load(ctx.sessionId);
    const off = offGroups(ctx.tools);
    const builtin = BUILTIN_TOOLS.filter((b) => !off.has(b.group)).map((b) => b.tool);
    const system: ChatMessage = {
      role: "system",
      content: `${toolsLine(builtin.map((t) => t.function.name))}${systemAppend(ctx.agentName, ctx.instructions)}`,
    };
    const mcpTools = await this.#opts.mcp.listTools({ signal: ctx.signal });
    const tools = [
      ...builtin,
      ...mcpTools.map((t) => ({
        type: "function" as const,
        function: {
          name: `${MCP_PREFIX}${t.name}`,
          description: t.description ?? "",
          parameters: plainSchema(t.inputSchema),
        },
      })),
    ];
    const messages: ChatMessage[] = [...history, { role: "user", content: turnPrompt(ctx) }];

    for (let call = 0; call < MAX_MODEL_CALLS; call++) {
      const reply = await this.#complete([system, ...messages], tools, ctx);
      if (reply.text.trim()) ctx.emit({ kind: "assistant_text", text: reply.text });
      if (reply.calls.length === 0) {
        messages.push({ role: "assistant", content: reply.text });
        // A message that came in while the answer was written gets its own reply.
        if (this.#late(ctx, messages)) continue;
        // Saved only when a turn completes: never a tool call without its result.
        await this.#save(id, messages);
        return { result: reply.text, sessionId: id };
      }
      messages.push({
        role: "assistant",
        content: reply.text,
        // Upstreams re-parse arguments on every request; a malformed one would fail the session for good.
        tool_calls: reply.calls.map((c) => ({
          id: c.id,
          type: "function",
          function: { name: c.name, arguments: JSON.stringify(c.input ?? {}) },
        })),
      });
      for (const c of reply.calls) {
        messages.push({ role: "tool", tool_call_id: c.id, content: await this.#runTool(c, ctx) });
      }
      this.#late(ctx, messages);
    }
    throw new Error(`turn stopped after ${MAX_MODEL_CALLS} model calls without a final answer`);
  }

  /** Adds messages that arrived during the turn; true if there were any. */
  #late(ctx: TurnContext, messages: ChatMessage[]): boolean {
    const late = ctx.takeMessages?.() ?? [];
    for (const m of late) {
      messages.push({
        role: "user",
        content: latePrompt(m),
      });
    }
    return late.length > 0;
  }

  async #complete(
    messages: ChatMessage[],
    tools: unknown[],
    ctx: TurnContext,
  ): Promise<{ text: string; calls: RequestedCall[] }> {
    const request = (withUsage: boolean): Promise<Response> =>
      this.#fetch(`${this.#opts.baseUrl.href.replace(/\/+$/, "")}/chat/completions`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${this.#opts.token}`,
        },
        // The daemon reads token usage from the last chunk; it has no choices, so the loop skips it.
        body: JSON.stringify({
          model: this.#opts.model,
          stream: true,
          ...(withUsage ? { stream_options: { include_usage: true } } : {}),
          messages,
          // Some models take no tools at all, not even an empty list.
          ...(tools.length > 0 ? { tools } : {}),
        }),
        signal: ctx.signal,
      });
    let res = await request(true);
    if (res.status === 400) {
      // Strict OpenAI-compatible servers reject `stream_options`; their usage then goes untracked.
      await res.body?.cancel();
      res = await request(false);
    }
    if (!res.ok || !res.body) {
      const why = `${String(res.status)} (${statusHint(res.status)})`;
      throw new Error(`The model provider answered ${why}: ${errorSnippet(await res.text())}`);
    }
    let text = "";
    const think = new ThinkFilter();
    const shown = (piece: string): void => {
      if (!piece) return;
      text += piece;
      ctx.emit({ kind: "assistant_delta", text: piece });
    };
    const parts: { id: string; name: string; args: string }[] = [];
    for await (const data of sseData(res.body)) {
      const chunk = JSON.parse(data) as StreamChunk;
      if (chunk.error !== undefined) throw new Error(`model stream failed: ${errorSnippet(data)}`);
      const delta = chunk.choices?.[0]?.delta;
      if (delta?.content) shown(think.push(delta.content));
      delta?.tool_calls?.forEach((piece, position) => {
        const part = (parts[piece.index ?? position] ??= { id: "", name: "", args: "" });
        part.id ||= piece.id ?? "";
        part.name ||= piece.function?.name ?? "";
        part.args += piece.function?.arguments ?? "";
      });
    }
    shown(think.finish());
    const calls = parts
      .filter((p) => p.name)
      .map((p) => ({
        id: p.id || `call_${randomUUID()}`,
        name: p.name,
        args: p.args,
        input: parseArgs(p.args),
      }));
    return { text, calls };
  }

  async #runTool(call: RequestedCall, ctx: TurnContext): Promise<string> {
    if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
    const { id, name, input } = call;
    if (input === null) {
      const output = `invalid JSON arguments for ${name}: ${call.args.slice(0, 200)}`;
      ctx.emit({ kind: "tool_use", id, name, input: call.args });
      ctx.emit({ kind: "tool_result", id, output, is_error: true });
      return output;
    }
    let verdict = this.#decide(name, input, offGroups(ctx.tools));
    const command = input["command"];
    if (verdict.allow && name === "bash" && this.#opts.gate)
      verdict = await this.#opts.gate.shell(typeof command === "string" ? command : "");
    if (!verdict.allow) {
      ctx.emit({ kind: "tool_denied", name, reason: verdict.reason });
      return verdict.reason;
    }
    ctx.emit({ kind: "tool_use", id, name, input });
    let outcome: ToolOutcome;
    try {
      outcome = await this.#execute(name, input, ctx.signal);
    } catch (err) {
      if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
      outcome = { output: errorMessage(err), isError: true };
    }
    ctx.emit({ kind: "tool_result", id, output: clip(outcome.output), is_error: outcome.isError });
    return clip(outcome.output, MAX_MODEL_TOOL_OUTPUT);
  }

  // Also refuses a built-in tool the model calls by name although it was not offered.
  #decide(name: string, input: Record<string, unknown>, off: ReadonlySet<BuiltinGroup>): Decision {
    const home = this.#opts.workspaceDir;
    const path = input["path"];
    const file_path = typeof path === "string" ? resolve(home, path) : undefined;
    switch (name) {
      case "bash":
        return decide("Bash", input, home, off);
      case "read_file":
        return decide("Read", { file_path }, home, off);
      case "write_file":
        return decide("Write", { file_path }, home, off);
      default:
        return decide(name, input, home, off);
    }
  }

  async #execute(
    name: string,
    input: Record<string, unknown>,
    signal: AbortSignal,
  ): Promise<ToolOutcome> {
    const home = this.#opts.workspaceDir;
    switch (name) {
      case "bash":
        return runBash(stringArg(input, "command"), home, signal);
      case "read_file":
        return {
          output: await readFile(resolve(home, stringArg(input, "path")), "utf8"),
          isError: false,
        };
      case "write_file": {
        const path = resolve(home, stringArg(input, "path"));
        const content = stringArg(input, "content");
        await mkdir(dirname(path), { recursive: true });
        await writeFile(path, content);
        return { output: `wrote ${content.length} characters to ${path}`, isError: false };
      }
    }
    if (!name.startsWith(MCP_PREFIX)) throw new Error(`unknown tool ${name}`);
    const result = await this.#opts.mcp.call(name.slice(MCP_PREFIX.length), input, {
      signal,
      timeout: MCP_CALL_TIMEOUT_MS,
    });
    return { output: JSON.stringify(result), isError: false };
  }

  #file(sessionId: string): string {
    return join(this.#opts.stateDir, `openai-${sessionId}.json`);
  }

  async #load(sessionId: string | null): Promise<{ id: string; history: ChatMessage[] }> {
    // The id names a file, so only plain ids are looked up.
    if (sessionId && SESSION_ID.test(sessionId)) {
      try {
        return {
          id: sessionId,
          history: JSON.parse(await readFile(this.#file(sessionId), "utf8")) as ChatMessage[],
        };
      } catch (err) {
        log.warn("could not resume session; starting a new one", {
          session: sessionId,
          error: errorMessage(err),
        });
      }
    }
    return { id: randomUUID(), history: [] };
  }

  // shortcut: the history grows without trimming; summarise or drop old turns once agents hit context limits.
  async #save(sessionId: string, messages: ChatMessage[]): Promise<void> {
    await mkdir(this.#opts.stateDir, { recursive: true, mode: 0o700 });
    const file = this.#file(sessionId);
    const tmp = `${file}.${process.pid}.tmp`;
    await writeFile(tmp, JSON.stringify(messages), { mode: 0o600 });
    await rename(tmp, file);
  }
}

async function* sseData(body: ReadableStream<Uint8Array>): AsyncGenerator<string> {
  const decoder = new TextDecoder();
  let buffer = "";
  for await (const chunk of body) {
    buffer += decoder.decode(chunk, { stream: true });
    const lines = buffer.split("\n");
    buffer = lines.pop() ?? "";
    for (const line of lines) {
      if (!line.startsWith("data:")) continue;
      const data = line.slice(5).trim();
      if (data === "[DONE]") return;
      if (data) yield data;
    }
  }
}

function parseArgs(args: string): Record<string, unknown> | null {
  try {
    const value: unknown = JSON.parse(args.trim() || "{}");
    return typeof value === "object" && value !== null && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function stringArg(input: Record<string, unknown>, key: string): string {
  const value = input[key];
  if (typeof value !== "string") throw new Error(`${key} must be a string`);
  return value;
}

/** The message of an OpenAI-style or daemon error envelope, else the start of the body. */
const STATUS_HINTS: Record<number, string> = {
  401: "the key was refused",
  403: "access was refused",
  404: "model or endpoint not found",
  429: "rate limit or quota reached",
};

const statusHint = (status: number): string =>
  STATUS_HINTS[status] ?? (status >= 500 ? "the provider had a problem" : "request refused");

function errorSnippet(body: string): string {
  try {
    const parsed = JSON.parse(body) as unknown;
    // Google wraps its error object in an array.
    const first = (Array.isArray(parsed) ? parsed[0] : parsed) as {
      error?: { message?: unknown };
    } | null;
    const message = first?.error?.message;
    if (typeof message === "string") return message;
  } catch {
    // not JSON
  }
  return body.slice(0, 300);
}

function runBash(command: string, cwd: string, signal: AbortSignal): Promise<ToolOutcome> {
  return new Promise((resolveOutcome, reject) => {
    // Own process group, so a timeout or abort also kills what the command started.
    const child = spawn("bash", ["-lc", command], {
      cwd,
      detached: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8").on("data", (s: string) => {
      if (stdout.length < MAX_CAPTURE) stdout += s;
    });
    child.stderr.setEncoding("utf8").on("data", (s: string) => {
      if (stderr.length < MAX_CAPTURE) stderr += s;
    });
    let timedOut = false;
    const kill = (): void => {
      try {
        if (child.pid) process.kill(-child.pid, "SIGKILL");
      } catch {
        // already gone
      }
    };
    const timer = setTimeout(() => {
      timedOut = true;
      kill();
    }, BASH_TIMEOUT_MS);
    signal.addEventListener("abort", kill, { once: true });
    const settle = (): void => {
      clearTimeout(timer);
      signal.removeEventListener("abort", kill);
    };
    child.on("error", (err) => {
      settle();
      reject(err);
    });
    child.on("close", (code, killedBy) => {
      settle();
      if (aborted(signal)) {
        reject(new TurnAbortedError("interrupted"));
        return;
      }
      const status = timedOut
        ? `timed out after ${BASH_TIMEOUT_MS / 1000} s`
        : code === null
          ? `killed by ${killedBy ?? "a signal"}`
          : `exit code ${code}`;
      resolveOutcome({
        output: `[${status}]\n${stdout}${stderr ? `\n[stderr]\n${stderr}` : ""}`,
        isError: code !== 0,
      });
    });
  });
}
