// Deterministic stand-in for a model. It exercises the real plumbing
// (protocol, MCP tools, mailbox) without an API key. Grammar:
//
//   @<agent> <text>         send <text> to <agent> via send_message
//   remember <key>: <text>  store a memory
//   recall <query>          search memories
//   anything else           echo; if it came from another agent (and is not
//                           already a reply), answer it with "re: <text>"
import type { FerrobotMcp } from "../mcp-client.ts";
import type { ModelDriver, TurnContext, TurnResult } from "./types.ts";
import { TurnAbortedError } from "./types.ts";

export type MockAction =
  | { kind: "send"; to: string; body: string }
  | { kind: "remember"; key: string; content: string }
  | { kind: "recall"; query: string }
  | { kind: "reply"; to: string; body: string }
  | { kind: "echo" };

/** Pure parser so the mock's behaviour is unit-testable. */
export function planMockAction(body: string, senderKind: "user" | "agent", senderName: string): MockAction {
  const text = body.trim();
  const send = /^@([a-z][a-z0-9-]{0,31})\s+([\s\S]+)$/.exec(text);
  if (send?.[1] && send[2]) return { kind: "send", to: send[1], body: send[2].trim() };
  const remember = /^remember\s+([^:]+):\s*([\s\S]+)$/i.exec(text);
  if (remember?.[1] && remember[2]) {
    return { kind: "remember", key: remember[1].trim(), content: remember[2].trim() };
  }
  const recall = /^recall\s+([\s\S]+)$/i.exec(text);
  if (recall?.[1]) return { kind: "recall", query: recall[1].trim() };
  if (senderKind === "agent" && !/^re:/i.test(text)) {
    return { kind: "reply", to: senderName, body: `re: ${text}` };
  }
  return { kind: "echo" };
}

export class MockDriver implements ModelDriver {
  readonly name = "mock";
  readonly #mcp: FerrobotMcp;

  constructor(mcp: FerrobotMcp) {
    this.#mcp = mcp;
  }

  async runTurn(ctx: TurnContext): Promise<TurnResult> {
    const { turn, emit } = ctx;
    const action = planMockAction(turn.body, turn.sender.kind, turn.sender_name);
    const tool = async (name: string, input: Record<string, unknown>): Promise<unknown> => {
      if (ctx.signal.aborted) throw new TurnAbortedError("interrupted");
      emit({ kind: "tool_use", name: `mcp__ferrobot__${name}`, input });
      return this.#mcp.call(name, input);
    };

    let result: string;
    switch (action.kind) {
      case "send":
        await tool("send_message", { to: action.to, body: action.body });
        result = `Sent "${action.body}" to ${action.to}.`;
        break;
      case "reply":
        await tool("send_message", { to: action.to, body: action.body, reply_to: turn.message_id });
        result = `Replied to ${action.to}.`;
        break;
      case "remember":
        await tool("remember", { key: action.key, content: action.content });
        result = `Remembered "${action.key}".`;
        break;
      case "recall": {
        const found = await tool("recall", { query: action.query });
        result = `Recall "${action.query}": ${JSON.stringify(found)}`;
        break;
      }
      case "echo":
        result = `[mock ${ctx.agentName}] got "${turn.body}" from ${turn.sender_name}`;
        break;
    }
    emit({ kind: "assistant_text", text: result });
    return { result, sessionId: ctx.sessionId ?? `mock-${ctx.agentName}` };
  }

  async close(): Promise<void> {
    await this.#mcp.close();
  }
}
