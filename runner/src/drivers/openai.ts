// Any OpenAI-compatible Chat Completions API (OpenAI, Gemini, OpenRouter,
// Ollama, ...). That API has no agent loop, so the driver runs one. Requests
// go to the daemon's model proxy with this agent's token; the real key and the
// upstream choice stay in the daemon.
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";

import { errorMessage, log } from "../log.ts";
import type { FerrobotMcp } from "../mcp-client.ts";
import { decide, type Decision } from "../permissions.ts";
import { aborted, clip, systemAppend, turnPrompt } from "./claude.ts";
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
      tool_calls?: { index?: number; id?: string; function?: { name?: string; arguments?: string } }[];
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
}

function functionTool(name: string, description: string, properties: Record<string, unknown>) {
  return {
    type: "function" as const,
    function: { name, description, parameters: { type: "object", properties, required: Object.keys(properties) } },
  };
}

const BUILTIN_TOOLS = [
  functionTool("bash", "Run a shell command (bash -lc) in your workspace. Times out after 120 s.", {
    command: { type: "string" },
  }),
  functionTool("read_file", "Read a text file. Relative paths are inside your workspace.", {
    path: { type: "string" },
  }),
  functionTool("write_file", "Write a text file, creating parent directories. Relative paths are inside your workspace.", {
    path: { type: "string" },
    content: { type: "string" },
  }),
];

const TOOLS_LINE = "On your own computer you can use the bash, read_file and write_file tools.";

/**
 * Inlines `$ref`s to `$defs` and drops `$schema`: several OpenAI-compatible
 * upstreams (Gemini among them) reject JSON Schema meta keywords.
 */
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
    const system: ChatMessage = {
      role: "system",
      content: `${TOOLS_LINE}\n${systemAppend(ctx.agentName, ctx.instructions)}`,
    };
    const mcpTools = await this.#opts.mcp.listTools({ signal: ctx.signal });
    const tools = [
      ...BUILTIN_TOOLS,
      ...mcpTools.map((t) => ({
        type: "function" as const,
        function: { name: `${MCP_PREFIX}${t.name}`, description: t.description ?? "", parameters: plainSchema(t.inputSchema) },
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
        content: `Message #${String(m.message_id)} from ${m.sender_name}, sent while you were working:\n\n${m.body}`,
      });
    }
    return late.length > 0;
  }

  async #complete(
    messages: ChatMessage[],
    tools: unknown[],
    ctx: TurnContext,
  ): Promise<{ text: string; calls: RequestedCall[] }> {
    const res = await this.#fetch(`${this.#opts.baseUrl.href.replace(/\/+$/, "")}/chat/completions`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${this.#opts.token}` },
      body: JSON.stringify({ model: this.#opts.model, stream: true, messages, tools }),
      signal: ctx.signal,
    });
    if (!res.ok || !res.body) {
      throw new Error(`model request failed with ${res.status}: ${errorSnippet(await res.text())}`);
    }
    let text = "";
    const parts: { id: string; name: string; args: string }[] = [];
    for await (const data of sseData(res.body)) {
      const chunk = JSON.parse(data) as StreamChunk;
      if (chunk.error !== undefined) throw new Error(`model stream failed: ${errorSnippet(data)}`);
      const delta = chunk.choices?.[0]?.delta;
      if (delta?.content) {
        text += delta.content;
        ctx.emit({ kind: "assistant_delta", text: delta.content });
      }
      delta?.tool_calls?.forEach((piece, position) => {
        const part = (parts[piece.index ?? position] ??= { id: "", name: "", args: "" });
        part.id ||= piece.id ?? "";
        part.name ||= piece.function?.name ?? "";
        part.args += piece.function?.arguments ?? "";
      });
    }
    const calls = parts
      .filter((p) => p.name)
      .map((p) => ({ id: p.id || `call_${randomUUID()}`, name: p.name, args: p.args, input: parseArgs(p.args) }));
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
    const verdict = this.#decide(name, input);
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

  #decide(name: string, input: Record<string, unknown>): Decision {
    const home = this.#opts.workspaceDir;
    const path = input["path"];
    const file_path = typeof path === "string" ? resolve(home, path) : undefined;
    switch (name) {
      case "bash":
        return decide("Bash", input, home);
      case "read_file":
        return decide("Read", { file_path }, home);
      case "write_file":
        return decide("Write", { file_path }, home);
      default:
        return decide(name, input, home);
    }
  }

  async #execute(name: string, input: Record<string, unknown>, signal: AbortSignal): Promise<ToolOutcome> {
    const home = this.#opts.workspaceDir;
    switch (name) {
      case "bash":
        return runBash(stringArg(input, "command"), home, signal);
      case "read_file":
        return { output: await readFile(resolve(home, stringArg(input, "path")), "utf8"), isError: false };
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
        return { id: sessionId, history: JSON.parse(await readFile(this.#file(sessionId), "utf8")) as ChatMessage[] };
      } catch (err) {
        log.warn("could not resume session; starting a new one", { session: sessionId, error: errorMessage(err) });
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
function errorSnippet(body: string): string {
  try {
    const message = (JSON.parse(body) as { error?: { message?: unknown } } | null)?.error?.message;
    if (typeof message === "string") return message;
  } catch {
    // not JSON
  }
  return body.slice(0, 300);
}

function runBash(command: string, cwd: string, signal: AbortSignal): Promise<ToolOutcome> {
  return new Promise((resolveOutcome, reject) => {
    // Own process group, so a timeout or abort also kills what the command started.
    const child = spawn("bash", ["-lc", command], { cwd, detached: true, stdio: ["ignore", "pipe", "pipe"] });
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
