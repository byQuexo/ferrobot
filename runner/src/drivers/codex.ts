// Codex (`codex app-server`) in the sandbox; its model traffic goes through the daemon's proxy.
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

import { errorMessage, log } from "../log.ts";
import type { ToolGate } from "../gate.ts";
import { decide, offGroups, type BuiltinGroup, type Decision } from "../permissions.ts";
import type { TurnEvent } from "../protocol.ts";
import { clip, systemAppend, toolResultText, turnPrompt } from "./claude.ts";
import { RpcProcess, spawnAgent, type Params } from "./rpc-process.ts";
import type { ModelDriver, TurnContext, TurnResult } from "./types.ts";
import { TurnAbortedError, throwIfAborted, whenAborted } from "./types.ts";

const PROVIDER = "ferrobot";
const TOKEN_ENV = "FERROBOT_AGENT_TOKEN";
// create_agent waits for the operator's decision (FERROBOT_APPROVAL_TIMEOUT, 300 s by default).
const MCP_TOOL_TIMEOUT_SEC = 600;

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
  gate?: ToolGate;
}

/** `$CODEX_HOME/config.toml`. Names the token's env var; never holds a secret. Tools of `off`
 * groups are not offered (checked against Codex 0.166's tool list). */
export function codexConfigToml(
  opts: { model: string; baseUrl: URL; mcpUrl: URL },
  off: ReadonlySet<BuiltinGroup> = new Set(),
): string {
  // A JSON string literal is also a valid TOML basic string.
  const str = (value: string): string => JSON.stringify(value);
  const features = [
    ...(off.has("shell") ? ["shell_tool = false", "unified_exec = false"] : []),
    ...(off.has("files") ? ["view_image = false"] : []),
    // Sub-agents run their own tools.
    ...(off.size > 0 ? ["multi_agent = false"] : []),
  ];
  return [
    `model = ${str(opts.model)}`,
    `model_provider = ${str(PROVIDER)}`,
    // Codex >= 0.166 rejects "untrusted"; requests it still sends are answered with decide().
    `approval_policy = "on-request"`,
    // The container is the boundary; Codex's own Landlock/Seatbelt may not work inside it.
    `sandbox_mode = "danger-full-access"`,
    ...(off.has("web") ? [`web_search = "disabled"`] : []),
    "",
    ...(features.length ? ["[features]", ...features, ""] : []),
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
  off: ReadonlySet<BuiltinGroup>,
): { name: string; verdict: Decision; command?: string } | null {
  switch (method) {
    case "item/commandExecution/requestApproval": {
      const command = typeof params["command"] === "string" ? params["command"] : "";
      return { name: "Bash", verdict: decide("Bash", { command }, home, off), command };
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
        .map((file_path) => decide("Write", { file_path }, home, off))
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

export class CodexDriver implements ModelDriver {
  readonly name = "codex";
  readonly #opts: CodexDriverOptions;
  #server: RpcProcess | null = null;
  #thread: { id: string; instructions: string } | null = null;
  #stale = false;
  /** The off groups the running app-server's config was written for. */
  #off = "";

  constructor(opts: CodexDriverOptions) {
    this.#opts = opts;
  }

  async runTurn(ctx: TurnContext): Promise<TurnResult> {
    throwIfAborted(ctx.signal);
    const server = await this.#start(offGroups(ctx.tools));
    const threadId = await this.#openThread(server, ctx);
    throwIfAborted(ctx.signal);
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

  async #start(off: ReadonlySet<BuiltinGroup>): Promise<RpcProcess> {
    const key = [...off].sort().join(",");
    if (this.#server?.alive && !this.#stale && this.#off === key) return this.#server;
    // A fresh app-server reads its tools and the MCP tools anew; the thread resumes by id.
    this.#server?.kill();
    this.#stale = false;
    this.#off = key;
    this.#thread = null;
    const home = join(this.#opts.stateDir, "codex");
    await mkdir(home, { recursive: true, mode: 0o700 });
    await writeFile(join(home, "config.toml"), codexConfigToml(this.#opts, off), { mode: 0o600 });
    const server = new RpcProcess(
      await spawnAgent("codex", this.#opts.bin, ["app-server"], this.#opts.workspaceDir, {
        ...process.env,
        CODEX_HOME: home,
        [TOKEN_ENV]: this.#opts.token,
      }),
      { name: "codex", process: "codex app-server", log: "codex", jsonrpc: false },
    );
    try {
      await server.request("initialize", { clientInfo: { name: "ferrobot", version: "0.1.11" } });
      server.notify("initialized");
    } catch (err) {
      server.kill();
      throw err;
    }
    this.#server = server;
    return server;
  }

  async #openThread(server: RpcProcess, ctx: TurnContext): Promise<string> {
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

  async #turn(server: RpcProcess, threadId: string, ctx: TurnContext): Promise<string | null> {
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
    const stopped = whenAborted(signal);

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
    server.onRequest = async (method, params) => {
      if (!ours(params)) return undefined;
      const reviewed = review(method, params, filesByItem, home, offGroups(ctx.tools));
      if (!reviewed) return undefined;
      let verdict = reviewed.verdict;
      if (verdict.allow && reviewed.command !== undefined && this.#opts.gate)
        verdict = await this.#opts.gate.shell(reviewed.command);
      if (verdict.allow) return { decision: "accept" };
      emit({ kind: "tool_denied", name: reviewed.name, reason: verdict.reason });
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
        await server.settleOrKill(server.request("turn/interrupt", { threadId, turnId }));
        throw new TurnAbortedError("interrupted");
      }
      if (end instanceof Error) throw end;
      if (end.status === "completed") return lastText;
      throwIfAborted(signal);
      throw new Error(end.error?.message ?? lastError ?? `codex turn ended as ${end.status}`);
    } finally {
      server.onNotification = () => {};
      server.onRequest = () => undefined;
    }
  }
}
