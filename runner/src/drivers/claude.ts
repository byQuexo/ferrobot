// Claude through the Claude Agent SDK. The SDK runs its own agent loop; we
// wire in the daemon's MCP server, a permission policy, an audit hook,
// interrupts and session resume.
import {
  query,
  type CanUseTool,
  type HookCallback,
  type Options,
  type SDKMessage,
} from "@anthropic-ai/claude-agent-sdk";

import { decide } from "../permissions.ts";
import type { SessionStore } from "../session.ts";
import type { ModelDriver, TurnContext, TurnResult } from "./types.ts";
import { TurnAbortedError } from "./types.ts";

export interface ClaudeDriverOptions {
  mcpUrl: URL;
  token: string;
  workspaceDir: string;
  model: string | undefined;
  session: SessionStore;
}

/** Text appended to the SDK's default system prompt. */
export function systemAppend(agentName: string, instructions: string): string {
  return [
    `You are "${agentName}", one agent in a ferrobot team. You have your own computer;`,
    "your working directory is your workspace and persists between turns.",
    "Coordinate with teammates through the ferrobot MCP tools: list_agents, send_message,",
    "read_mailbox, remember, recall, forget. Messages are asynchronous: after sending one,",
    "finish your turn; the answer arrives later as a new message.",
    instructions.trim() ? `\nYour role:\n${instructions.trim()}` : "",
  ].join("\n");
}

export function turnPrompt(ctx: Pick<TurnContext, "turn">): string {
  const { turn } = ctx;
  return `Message #${String(turn.message_id)} from ${turn.sender_name}:\n\n${turn.body}`;
}

export class ClaudeDriver implements ModelDriver {
  readonly name = "claude";
  readonly #opts: ClaudeDriverOptions;

  constructor(opts: ClaudeDriverOptions) {
    this.#opts = opts;
  }

  async runTurn(ctx: TurnContext): Promise<TurnResult> {
    const { emit } = ctx;
    const home = this.#opts.workspaceDir;
    const abortController = new AbortController();
    const onAbort = (): void => { abortController.abort(); };
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

    const options: Options = {
      cwd: home,
      abortController,
      canUseTool,
      permissionMode: "default",
      settingSources: [],
      systemPrompt: {
        type: "preset",
        preset: "claude_code",
        append: systemAppend(ctx.agentName, ctx.instructions),
      },
      mcpServers: {
        ferrobot: {
          type: "http",
          url: this.#opts.mcpUrl.toString(),
          headers: { Authorization: `Bearer ${this.#opts.token}` },
        },
      },
      hooks: { PreToolUse: [{ hooks: [audit] }] },
      ...(ctx.sessionId ? { resume: ctx.sessionId } : {}),
      ...(this.#opts.model ? { model: this.#opts.model } : {}),
    };

    let sessionId = ctx.sessionId;
    let result: string | null = null;
    try {
      for await (const message of query({ prompt: turnPrompt(ctx), options })) {
        sessionId = handleMessage(message, emit) ?? sessionId;
        if (message.type === "result") {
          if (message.subtype === "success") {
            result = message.result;
          } else {
            throw new Error(`claude turn ended with ${message.subtype}`);
          }
        }
      }
    } catch (err) {
      if (ctx.signal.aborted) throw new TurnAbortedError("interrupted");
      throw err;
    } finally {
      ctx.signal.removeEventListener("abort", onAbort);
    }
    return { result, sessionId };
  }

  async close(): Promise<void> {
    // query() instances end with their turn; nothing long-lived to close.
  }
}

/** Maps SDK messages to turn events. Returns the session id if present. */
function handleMessage(message: SDKMessage, emit: TurnContext["emit"]): string | null {
  if (message.type === "assistant") {
    for (const block of message.message.content) {
      if (block.type === "text" && block.text.trim()) {
        emit({ kind: "assistant_text", text: block.text });
      } else if (block.type === "tool_use") {
        emit({ kind: "tool_use", name: block.name, input: block.input });
      }
    }
  }
  return "session_id" in message && typeof message.session_id === "string"
    ? message.session_id
    : null;
}
