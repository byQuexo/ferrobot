import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { ClaudeCodeDriver } from "../src/drivers/claude-code.ts";
import { context } from "./helpers.ts";

// A real `claude -p` stream-json turn, trimmed to the fields the driver reads.
const TURN = readFileSync(new URL("fixtures/claude-code-turn.jsonl", import.meta.url), "utf8");

/** Answers like the daemon's turn endpoint, in uneven chunks. */
function daemon(
  status: number,
  body: string,
): { fetch: typeof fetch; sent: RequestInit[]; urls: string[] } {
  const sent: RequestInit[] = [];
  const urls: string[] = [];
  const fake = (url: URL, init?: RequestInit): Promise<Response> => {
    urls.push(url.href);
    sent.push(init ?? {});
    const bytes = new TextEncoder().encode(body);
    const stream = new ReadableStream<Uint8Array>({
      start(c) {
        for (let i = 0; i < bytes.length; i += 97) c.enqueue(bytes.slice(i, i + 97));
        c.close();
      },
    });
    return Promise.resolve(new Response(stream, { status }));
  };
  return { fetch: fake as unknown as typeof fetch, sent, urls };
}

const driver = (f: typeof fetch): ClaudeCodeDriver =>
  new ClaudeCodeDriver({
    daemonUrl: new URL("http://host.docker.internal:7879"),
    token: "agent-token",
    fetch: f,
  });

describe("ClaudeCodeDriver", () => {
  it("hands the turn to the host's claude and maps its stream", async () => {
    const d = daemon(200, TURN);
    const { ctx, events } = context();
    const out = await driver(d.fetch).runTurn(ctx);
    assert.deepEqual(out, { result: "OK", sessionId: "e91ba174-e016-40e1-b41e-1e478eb267e4" });
    assert.deepEqual(
      events.map((e) => e.kind),
      ["assistant_delta", "assistant_text"],
    );
    assert.equal(d.urls[0], "http://host.docker.internal:7879/host/claude-code/turn");
    const init = d.sent[0];
    assert.equal((init?.headers as Record<string, string>)["Authorization"], "Bearer agent-token");
    const body = JSON.parse(init?.body as string) as {
      prompt: string;
      system: string;
      new_session: boolean;
    };
    assert.equal(body.prompt, "Message #7 from user:\n\nhi");
    assert.match(body.system, /bash, read_file and write_file/);
    assert.equal(body.new_session, true, "no session yet: the daemon starts one");
  });

  it("fails the turn with claude's own reason", async () => {
    const failed =
      '{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}\n';
    await assert.rejects(
      driver(daemon(200, failed).fetch).runTurn(context({ sessionId: "s1" }).ctx),
      /Not logged in/,
    );
    await assert.rejects(
      driver(daemon(403, '{"error":"this agent does not run on Claude Code"}').fetch).runTurn(
        context({ sessionId: "s1" }).ctx,
      ),
      /answered 403: .*does not run on Claude Code/,
    );
  });
});
