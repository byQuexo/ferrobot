// Claude through the Claude Agent SDK. The SDK runs its own agent loop; we
// wire in the daemon's MCP server, a permission policy, an audit hook,
// streaming, interrupts and session resume.
//
// Model traffic: the daemon starts the sandbox with ANTHROPIC_BASE_URL
// pointing at its model proxy and ANTHROPIC_API_KEY set to this agent's
// token, so the real key never enters the sandbox. The SDK reads both from
// the environment.
import {
  query as sdkQuery,
  type CanUseTool,
  type HookCallback,
  type Options,
  type SDKMessage,
} from "@anthropic-ai/claude-agent-sdk";

import { log } from "../log.ts";
import { decide } from "../permissions.ts";
import type { TurnEvent } from "../protocol.ts";
import type { SessionStore } from "../session.ts";
import type { ModelDriver, TurnContext, TurnResult } from "./types.ts";
import { TurnAbortedError } from "./types.ts";

/** Tool output beyond this many characters is cut before it goes on the wire. */
export const MAX_TOOL_OUTPUT = 4000;

/** The subset of the SDK's `query` the driver uses (injectable for tests). */
export type QueryFn = (args: { prompt: string; options: Options }) => AsyncIterable<SDKMessage>;

export interface ClaudeDriverOptions {
  mcpUrl: URL;
  token: string;
  workspaceDir: string;
  model: string | undefined;
  session: SessionStore;
  query?: QueryFn;
}

/** Text appended to the SDK's default system prompt. */
export function systemAppend(agentName: string, instructions: string): string {
  return [
    `You are "${agentName}", one agent in a ferrobot team. You have your own computer;`,
    "your working directory is your workspace and persists between turns.",
    "Coordinate with teammates through the ferrobot MCP tools: list_agents, send_message,",
    "read_mailbox, remember, recall, forget, ask_user (a question card for your operator)",
    "and create_agent (a new teammate; waits for your operator's approval).",
    "Messages and answers are asynchronous: after sending a message or asking,",
    "finish your turn; the answer arrives later as a new message. Address teammates by",
    "the id from list_agents (names can change). Messages from",
    '"routine:<name>" are scheduled tasks set up by your operator.',
    instructions.trim() ? `\nYour role:\n${instructions.trim()}` : "",
  ].join("\n");
}

export function turnPrompt(ctx: Pick<TurnContext, "turn">): string {
  const { turn } = ctx;
  return `Message #${String(turn.message_id)} from ${turn.sender_name}:\n\n${turn.body}`;
}

/** Builds the SDK options for one turn (pure; checked against the SDK's `Options`). */
export function buildOptions(args: {
  ctx: Pick<TurnContext, "agentName" | "instructions">;
  workspaceDir: string;
  mcpUrl: URL;
  token: string;
  model: string | undefined;
  resume: string | null;
  abortController: AbortController;
  canUseTool: CanUseTool;
  audit: HookCallback;
}): Options {
  return {
    cwd: args.workspaceDir,
    abortController: args.abortController,
    canUseTool: args.canUseTool,
    permissionMode: "default",
    settingSources: [],
    includePartialMessages: true,
    systemPrompt: {
      type: "preset",
      preset: "claude_code",
      append: systemAppend(args.ctx.agentName, args.ctx.instructions),
    },
    mcpServers: {
      ferrobot: {
        type: "http",
        url: args.mcpUrl.toString(),
        headers: { Authorization: `Bearer ${args.token}` },
      },
    },
    hooks: { PreToolUse: [{ hooks: [args.audit] }] },
    ...(args.resume ? { resume: args.resume } : {}),
    ...(args.model ? { model: args.model } : {}),
  };
}

/** What one SDK message means for the turn. */
export interface Mapped {
  events: TurnEvent[];
  sessionId: string | null;
  /** Set by the final `result` message. */
  outcome?: { ok: true; result: string } | { ok: false; error: string };
}

export function clip(text: string, max = MAX_TOOL_OUTPUT): string {
  return text.length > max
    ? `${text.slice(0, max)}\n[... ${String(text.length - max)} more characters]`
    : text;
}

/** Text of a `tool_result` block's content (string or content blocks). */
export function toolResultText(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .map((block: unknown) => {
      if (typeof block !== "object" || block === null) return "";
      const b = block as { type?: unknown; text?: unknown };
      if (b.type === "text" && typeof b.text === "string") return b.text;
      return `[${typeof b.type === "string" ? b.type : "content"}]`;
    })
    .join("\n");
}

/** Maps one SDK message to turn events. Pure, so it is tested against SDK-typed fixtures. */
export function mapSdkMessage(message: SDKMessage): Mapped {
  const events: TurnEvent[] = [];
  const out: Mapped = {
    events,
    sessionId: "session_id" in message && typeof message.session_id === "string" ? message.session_id : null,
  };
  if (message.type === "stream_event") {
    // Live text of the top-level assistant only; subagent chatter stays out.
    const ev = message.event;
    if (
      message.parent_tool_use_id === null &&
      ev.type === "content_block_delta" &&
      ev.delta.type === "text_delta" &&
      ev.delta.text
    ) {
      events.push({ kind: "assistant_delta", text: ev.delta.text });
    }
  } else if (message.type === "assistant") {
    for (const block of message.message.content) {
      if (block.type === "text" && block.text.trim() && message.parent_tool_use_id === null) {
        events.push({ kind: "assistant_text", text: block.text });
      } else if (block.type === "tool_use" || block.type === "mcp_tool_use") {
        events.push({ kind: "tool_use", id: block.id, name: block.name, input: block.input });
      }
    }
  } else if (message.type === "user") {
    const content = message.message.content;
    if (typeof content !== "string") {
      for (const block of content) {
        if (block.type === "tool_result") {
          events.push({
            kind: "tool_result",
            id: block.tool_use_id,
            output: clip(toolResultText(block.content)),
            is_error: block.is_error === true,
          });
        }
      }
    }
  } else if (message.type === "result") {
    out.outcome =
      message.subtype === "success"
        ? { ok: true, result: message.result }
        : { ok: false, error: `claude turn ended with ${message.subtype}` };
  }
  return out;
}

export class ClaudeDriver implements ModelDriver {
  readonly name = "claude";
  readonly #opts: ClaudeDriverOptions;
  readonly #query: QueryFn;

  constructor(opts: ClaudeDriverOptions) {
    this.#opts = opts;
    this.#query = opts.query ?? sdkQuery;
  }

  async runTurn(ctx: TurnContext): Promise<TurnResult> {
    try {
      return await this.#attempt(ctx, ctx.sessionId);
    } catch (err) {
      // A session that cannot be resumed (e.g. after restoring an older
      // snapshot) must not wedge the agent: start a fresh one once.
      if (err instanceof ResumeFailedError) {
        log.warn("could not resume session; starting a new one", { session: ctx.sessionId });
        return this.#attempt(ctx, null);
      }
      throw err;
    }
  }

  async #attempt(ctx: TurnContext, resume: string | null): Promise<TurnResult> {
    const { emit } = ctx;
    const home = this.#opts.workspaceDir;
    const abortController = new AbortController();
    const onAbort = (): void => { abortController.abort(); };
    if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
    ctx.signal.addEventListener("abort", onAbort, { once: true });

    const canUseTool: CanUseTool = (toolName, input) => {
      const verdict = decide(toolName, input, home);
      if (verdict.allow) return Promise.resolve({ behavior: "allow", updatedInput: input });
      emit({ kind: "tool_denied", name: toolName, reason: verdict.reason });
      return Promise.resolve({ behavior: "deny", message: verdict.reason });
    };
    const audit: HookCallback = async (input) => {
      if (input.hook_event_name === "PreToolUse") {
        await this.#opts.session.audit({ tool: input.tool_name, input: input.tool_input });
      }
      return { continue: true };
    };
    const options = buildOptions({
      ctx,
      workspaceDir: home,
      mcpUrl: this.#opts.mcpUrl,
      token: this.#opts.token,
      model: this.#opts.model,
      resume,
      abortController,
      canUseTool,
      audit,
    });

    let sessionId = resume;
    let result: string | null = null;
    let emitted = 0;
    try {
      for await (const message of this.#query({ prompt: turnPrompt(ctx), options })) {
        const mapped = mapSdkMessage(message);
        sessionId = mapped.sessionId ?? sessionId;
        for (const event of mapped.events) {
          emitted += 1;
          emit(event);
        }
        if (mapped.outcome?.ok === true) result = mapped.outcome.result;
        if (mapped.outcome?.ok === false) {
          if (resume && emitted === 0) throw new ResumeFailedError(mapped.outcome.error);
          throw new Error(mapped.outcome.error);
        }
      }
    } catch (err) {
      if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
      if (resume && emitted === 0 && !(err instanceof ResumeFailedError)) {
        throw new ResumeFailedError(err instanceof Error ? err.message : String(err));
      }
      throw err;
    } finally {
      ctx.signal.removeEventListener("abort", onAbort);
    }
    if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
    return { result, sessionId };
  }

  async close(): Promise<void> {
    // query() instances end with their turn; nothing long-lived to close.
  }
}

/** Reads the live flag (a function call, so control-flow narrowing does not stick). */
export function aborted(signal: AbortSignal): boolean {
  return signal.aborted;
}

class ResumeFailedError extends Error {
  override name = "ResumeFailedError";
}
