// Deterministic stand-in for a model that drives the real plumbing without a key. Grammar (seed-demo.sh,
// smoke tests): `@agent text`, `remember key: text`, `recall query`, `ask Q: a | b; Q+: a | ...; Q`
// (single, multiple with custom answer, free text); else echo, and agents get one "re: text" reply.
import type { FerrobotMcp } from "../mcp-client.ts";
import type { ModelDriver, TurnContext, TurnResult } from "./types.ts";
import { throwIfAborted } from "./types.ts";

export type MockAction =
  | { kind: "send"; to: string; body: string }
  | { kind: "remember"; key: string; content: string }
  | { kind: "recall"; query: string }
  | { kind: "ask"; questions: MockQuestion[] }
  | { kind: "reply"; to: string; body: string }
  | { kind: "echo" };

export interface MockQuestion {
  prompt: string;
  kind: "single" | "multi" | "text";
  choices: { label: string }[];
  allow_custom: boolean;
}

function parseQuestion(text: string): MockQuestion {
  const m = /^([^:]+?)(\+)?:\s*(.+)$/.exec(text.trim());
  if (!m?.[1] || !m[3])
    return { prompt: text.trim(), kind: "text", choices: [], allow_custom: false };
  const labels = m[3]
    .split("|")
    .map((s) => s.trim())
    .filter(Boolean);
  const custom = labels.at(-1) === "...";
  return {
    prompt: m[1].trim(),
    kind: m[2] ? "multi" : "single",
    choices: (custom ? labels.slice(0, -1) : labels).map((label) => ({ label })),
    allow_custom: custom,
  };
}

/** Parses a message body into the mock's action. */
export function planMockAction(
  body: string,
  senderKind: "user" | "agent" | "routine",
  senderName: string,
): MockAction {
  const text = body.trim();
  const send = /^@([a-z][a-z0-9-]{0,31})\s+([\s\S]+)$/.exec(text);
  if (send?.[1] && send[2]) return { kind: "send", to: send[1], body: send[2].trim() };
  const remember = /^remember\s+([^:]+):\s*([\s\S]+)$/i.exec(text);
  if (remember?.[1] && remember[2]) {
    return { kind: "remember", key: remember[1].trim(), content: remember[2].trim() };
  }
  const ask = /^ask\s+([\s\S]+)$/i.exec(text);
  if (ask?.[1])
    return {
      kind: "ask",
      questions: ask[1]
        .split(";")
        .filter((q) => q.trim())
        .map(parseQuestion),
    };
  const recall = /^recall\s+([\s\S]+)$/i.exec(text);
  if (recall?.[1]) return { kind: "recall", query: recall[1].trim() };
  if (senderKind === "agent" && !/^re:/i.test(text)) {
    return { kind: "reply", to: senderName, body: `re: ${text}` };
  }
  return { kind: "echo" };
}

/** Splits text into word-sized streaming fragments that concatenate back to it. */
export function streamPieces(text: string): string[] {
  return text.match(/\S+\s*|\s+/g) ?? [];
}

export class MockDriver implements ModelDriver {
  readonly name = "mock";
  readonly #mcp: FerrobotMcp;

  constructor(mcp: FerrobotMcp) {
    this.#mcp = mcp;
  }

  async runTurn(ctx: TurnContext): Promise<TurnResult> {
    const { turn, emit } = ctx;
    // Replies go to the sender's id: names can change, ids cannot.
    const replyTo = turn.sender.kind === "agent" ? turn.sender.id : turn.sender_name;
    const action = planMockAction(turn.body, turn.sender.kind, replyTo);
    let calls = 0;
    const tool = async (name: string, input: Record<string, unknown>): Promise<unknown> => {
      throwIfAborted(ctx.signal);
      calls += 1;
      const id = `mock_${turn.turn_id}_${String(calls)}`;
      emit({ kind: "tool_use", id, name: `mcp__ferrobot__${name}`, input });
      try {
        const output = await this.#mcp.call(name, input);
        emit({ kind: "tool_result", id, output: JSON.stringify(output), is_error: false });
        return output;
      } catch (err) {
        emit({ kind: "tool_result", id, output: String(err), is_error: true });
        throw err;
      }
    };

    let result: string;
    switch (action.kind) {
      case "send":
        await tool("send_message", { to: action.to, body: action.body });
        result = `Sent "${action.body}" to ${action.to}.`;
        break;
      case "reply":
        await tool("send_message", { to: action.to, body: action.body, reply_to: turn.message_id });
        result = `Replied to ${turn.sender_name}.`;
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
      case "ask":
        await tool("ask_user", { questions: action.questions });
        result = "I put a few questions in front of you above.";
        break;
      case "echo":
        result = `[mock ${ctx.agentName}] got "${turn.body}" from ${turn.sender_name}`;
        break;
    }
    for (const piece of streamPieces(result)) {
      throwIfAborted(ctx.signal);
      emit({ kind: "assistant_delta", text: piece });
    }
    emit({ kind: "assistant_text", text: result });
    return { result, sessionId: ctx.sessionId ?? `mock-${ctx.agentName}` };
  }

  async close(): Promise<void> {
    await this.#mcp.close();
  }
}
