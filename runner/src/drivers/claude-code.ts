// The daemon host's own `claude` CLI runs the model, signed in with the operator's own Claude login;
// this driver only hands it the turn and maps its stream-json output. Nothing of it is in the sandbox.
import type { SDKMessage } from "@anthropic-ai/claude-agent-sdk";

import { mapSdkMessage, systemAppend, turnPrompt } from "./claude.ts";
import type { ModelDriver, TurnContext, TurnResult } from "./types.ts";
import { TurnAbortedError } from "./types.ts";

const COMPUTER = [
  "Your computer is reached through the bash, read_file and write_file tools; they act in your",
  "workspace. You have no other way to run commands or touch files.",
].join("\n");

export interface ClaudeCodeDriverOptions {
  daemonUrl: URL;
  token: string;
  fetch?: typeof fetch;
}

async function* lines(body: ReadableStream<Uint8Array>): AsyncGenerator<string> {
  let rest = "";
  for await (const chunk of body.pipeThrough(new TextDecoderStream())) {
    const parts = (rest + chunk).split("\n");
    rest = parts.pop() ?? "";
    yield* parts.filter((l) => l.trim());
  }
  if (rest.trim()) yield rest;
}

export class ClaudeCodeDriver implements ModelDriver {
  readonly name = "claude_code";
  readonly #opts: ClaudeCodeDriverOptions;

  constructor(opts: ClaudeCodeDriverOptions) {
    this.#opts = opts;
  }

  async runTurn(ctx: TurnContext): Promise<TurnResult> {
    try {
      return await this.#turn(ctx);
    } catch (err) {
      if (ctx.signal.aborted) throw new TurnAbortedError("interrupted");
      throw err;
    }
  }

  async #turn(ctx: TurnContext): Promise<TurnResult> {
    const res = await (this.#opts.fetch ?? fetch)(
      new URL("/host/claude-code/turn", this.#opts.daemonUrl),
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${this.#opts.token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          prompt: turnPrompt(ctx),
          system: `${systemAppend(ctx.agentName, ctx.instructions)}\n\n${COMPUTER}`,
          new_session: ctx.sessionId === null,
        }),
        signal: ctx.signal,
      },
    );
    if (!res.ok || !res.body) {
      const body = (await res.text()).slice(0, 300);
      throw new Error(`Claude Code answered ${String(res.status)}: ${body}`);
    }
    let sessionId = ctx.sessionId;
    let outcome: { ok: true; result: string } | { ok: false; error: string } | undefined;
    for await (const line of lines(res.body)) {
      const message = JSON.parse(line) as SDKMessage;
      const mapped = mapSdkMessage(message);
      mapped.events.forEach((e) => {
        ctx.emit(e);
      });
      sessionId = mapped.sessionId ?? sessionId;
      // A failed login or limit ends with `is_error` and the reason as the result text.
      if (message.type === "result" && message.is_error) {
        const text = "result" in message ? message.result : "";
        outcome = { ok: false, error: text || `claude turn ended with ${message.subtype}` };
      } else {
        outcome = mapped.outcome ?? outcome;
      }
    }
    if (!outcome) throw new Error("Claude Code ended without a result");
    if (!outcome.ok) throw new Error(outcome.error);
    return { result: outcome.result, sessionId };
  }

  async close(): Promise<void> {}
}
