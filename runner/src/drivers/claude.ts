// Claude through the Claude Agent SDK, which runs its own agent loop. The SDK reads ANTHROPIC_BASE_URL
// (the daemon's model proxy) and ANTHROPIC_API_KEY (this agent's token) from the env: no real key here.
import { randomUUID } from "node:crypto";

import {
  query as sdkQuery,
  type CanUseTool,
  type HookCallback,
  type Options,
  type SDKMessage,
  type SDKUserMessage,
} from "@anthropic-ai/claude-agent-sdk";

import { log } from "../log.ts";
import { CLAUDE_SUBAGENT_TOOLS, CLAUDE_TOOLS, decide, offGroups } from "../permissions.ts";
import type { SteerFrame, TurnEvent } from "../protocol.ts";
import type { SessionStore } from "../session.ts";
import type { ModelDriver, TurnContext, TurnResult } from "./types.ts";
import { TurnAbortedError } from "./types.ts";

/** Tool output beyond this many characters is cut before it goes on the wire. */
export const MAX_TOOL_OUTPUT = 4000;

/** The subset of the SDK's `query` the driver uses (injectable for tests). */
export type QueryFn = (args: {
  prompt: AsyncIterable<SDKUserMessage>;
  options: Options;
}) => AsyncIterable<SDKMessage>;

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
    "assign_task, list_tasks, read_mailbox, remember, recall, forget, ask_user (a question",
    "card for your operator), create_agent (a new teammate; waits for your operator's",
    "approval unless Auto mode is on), create_routine (a message to yourself on a schedule, for",
    "recurring work or reminders) and add_connector (asks your operator to add a remote",
    "MCP server; a token goes into a hidden field, so never ask for keys or tokens in chat).",
    "Messages and answers are asynchronous: after sending a message or asking,",
    "finish your turn; the answer arrives later as a new message. A message wakes a",
    "sleeping teammate, so you can always reach one. In send_message.to use",
    "the id from list_agents (names can change). In any text you write, mention a teammate",
    "as @name (e.g. @bob), never bold, in backticks or by id: your operator's app shows it",
    "as a link to that agent.",
    "To hand a teammate work, use assign_task, or create_agent with a task for a new one:",
    "when its turn on the task ends, its answer comes back to you automatically, even while",
    "you sleep. list_tasks shows the tasks you gave and got. Give a new agent a short,",
    "memorable lowercase name that fits its role, like scout (research), forge (building),",
    "ledger (numbers) or echo (comms), not research-agent.",
    "A teammate's task is yours to finish: end your turn with your answer and it goes back",
    "to that teammate automatically (a send_message with reply_to the task replaces it, so",
    "use reply_to only for your final answer). Send questions to that teammate with",
    "send_message, not ask_user; it talks to the operator. When a",
    "teammate you briefed asks something only the operator knows, ask with ask_user, then",
    "pass the answer back to it with send_message.",
    "Share what you learn as you go: after each useful step, write a short update with",
    "your findings so far (in your reply for your operator, or with send_message without",
    "reply_to to the teammate that gave you the task), then keep working. Do not save it",
    "all for the end.",
    'A message marked "sent while you were working" came in during your task: acknowledge',
    'it at once in one short line (e.g. "Noted, I\'ll finish the flights first" or "Good',
    "point, I'll tell @bob\"), then act on it or carry on.",
    "To hand your operator a file you made (a plan, a report), call share_file with its",
    "path; it shows in your chat as a file they can open. For something visual (a page, a",
    "chart, a dashboard), write one self-contained .html file and share it: it opens as a",
    "page inside ferrobot. For a PDF, write the document as HTML and run",
    "`ferrobot-pdf page.html out.pdf`, then share the PDF.",
    "Messages from",
    '"routine:<name>" are scheduled tasks set up by your operator.',
    instructions.trim() ? `\nYour role:\n${instructions.trim()}` : "",
  ].join("\n");
}

export function turnPrompt(ctx: Pick<TurnContext, "turn">): string {
  const { turn } = ctx;
  return `Message #${String(turn.message_id)} from ${turn.sender_name}:\n\n${turn.body}`;
}

/** A message that arrived during the turn, as the model sees it. */
export function latePrompt(m: SteerFrame): string {
  return `Message #${String(m.message_id)} from ${m.sender_name}, sent while you were working:\n\n${m.body}`;
}

/** The SDK's streaming input for one turn; ends with `end()`. */
class Inbox implements AsyncIterable<SDKUserMessage> {
  #items: SDKUserMessage[] = [];
  #wake: (() => void) | null = null;
  #ended = false;

  /** Queues a user message and returns its uuid. */
  push(text: string): string {
    const uuid = randomUUID();
    // client_composed: delivered as written, no slash commands or @-mentions from another agent's text;
    // priority "next" folds the message into the running turn between tool rounds.
    this.#items.push({
      type: "user",
      message: { role: "user", content: text },
      parent_tool_use_id: null,
      uuid,
      priority: "next",
      client_composed: true,
    });
    this.#wake?.();
    return uuid;
  }

  end(): void {
    this.#ended = true;
    this.#wake?.();
  }

  async *[Symbol.asyncIterator](): AsyncIterator<SDKUserMessage> {
    for (;;) {
      const next = this.#items.shift();
      if (next) {
        yield next;
        continue;
      }
      if (this.#ended) return;
      await new Promise<void>((resolve) => {
        this.#wake = resolve;
      });
      this.#wake = null;
    }
  }
}

/** Builds the SDK options for one turn (pure; checked against the SDK's `Options`). */
export function buildOptions(args: {
  ctx: Pick<TurnContext, "agentName" | "instructions" | "tools">;
  workspaceDir: string;
  mcpUrl: URL;
  token: string;
  model: string | undefined;
  resume: string | null;
  abortController: AbortController;
  canUseTool: CanUseTool;
  audit: HookCallback;
}): Options {
  const off = [...offGroups(args.ctx.tools)];
  // Removed from the model's context, not just refused when called.
  const disallowed = off.length
    ? [...off.flatMap((g) => CLAUDE_TOOLS[g]), ...CLAUDE_SUBAGENT_TOOLS]
    : [];
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
    ...(disallowed.length ? { disallowedTools: disallowed } : {}),
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

/** Maps one SDK message to turn events. */
export function mapSdkMessage(message: SDKMessage): Mapped {
  const events: TurnEvent[] = [];
  const out: Mapped = {
    events,
    sessionId:
      "session_id" in message && typeof message.session_id === "string" ? message.session_id : null,
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
  readonly steering = true;
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
      // A session that cannot be resumed (e.g. after restoring an older snapshot) must not wedge the agent.
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
    const onAbort = (): void => {
      abortController.abort();
    };
    if (aborted(ctx.signal)) throw new TurnAbortedError("interrupted");
    ctx.signal.addEventListener("abort", onAbort, { once: true });

    const off = offGroups(ctx.tools);
    const canUseTool: CanUseTool = (toolName, input) => {
      const verdict = decide(toolName, input, home, off);
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
    const inbox = new Inbox();
    const first = inbox.push(turnPrompt(ctx));
    // Every message pushed must be answered before the query may end.
    const open = new Set([first]);
    const steer = (): void => {
      for (const m of ctx.takeMessages?.() ?? []) open.add(inbox.push(latePrompt(m)));
    };
    try {
      for await (const message of this.#query({ prompt: inbox, options })) {
        if (message.type === "result") {
          // Any result answers the turn's prompt; only late messages can hold the query open.
          open.delete(first);
          const consumed = message.user_message_uuids;
          if (consumed) for (const id of consumed) open.delete(id);
          // An older CLI does not say what it consumed: take it as all.
          else open.clear();
        }
        steer();
        if (message.type === "result" && open.size === 0) inbox.end();
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
      inbox.end();
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
