import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { describe, it } from "node:test";

import { ConfigError, loadConfig } from "../src/config.ts";
import { OpenAIDriver, plainSchema, type ChatMessage } from "../src/drivers/openai.ts";
import { TurnAbortedError, type TurnContext } from "../src/drivers/types.ts";
import type { FerrobotMcp } from "../src/mcp-client.ts";
import type { TurnEvent } from "../src/protocol.ts";

type Chunk = Record<string, unknown>;

interface Sent {
  url: string;
  headers: Record<string, string>;
  body: {
    model: string;
    stream: boolean;
    messages: ChatMessage[];
    tools: { type: string; function: { name: string; description: string; parameters: unknown } }[];
  };
}

const text = (content: string): Chunk => ({ choices: [{ delta: { content } }] });
const call = (index: number, fn: { name?: string; arguments?: string }, id?: string): Chunk => ({
  choices: [{ delta: { tool_calls: [{ index, ...(id ? { id } : {}), function: fn }] } }],
});

/** An SSE response cut into 7-byte pieces, so events and characters split across reads. */
function sse(chunks: Chunk[]): Response {
  const bytes = new TextEncoder().encode(
    `: keepalive\n\n${chunks.map((c) => `data: ${JSON.stringify(c)}\n\n`).join("")}data: [DONE]\n\n`,
  );
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (let i = 0; i < bytes.length; i += 7) controller.enqueue(bytes.slice(i, i + 7));
      controller.close();
    },
  });
  return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
}

function at<T>(items: readonly T[], index: number): T {
  const item = items.at(index);
  assert.ok(item !== undefined, `no item at ${index}`);
  return item;
}

interface Setup {
  replies?: (Chunk[] | Response)[];
  tools?: unknown[];
  stateDir?: string;
  fetch?: typeof fetch;
  mcpCall?: (name: string, args: Record<string, unknown>) => Promise<unknown>;
}

function setup(opts: Setup = {}) {
  const workspaceDir = mkdtempSync(join(tmpdir(), "ferrobot-ws-"));
  const stateDir = opts.stateDir ?? mkdtempSync(join(tmpdir(), "ferrobot-state-"));
  const sent: Sent[] = [];
  const replies = [...(opts.replies ?? [])];
  const fakeFetch = ((url: string, init: RequestInit) => {
    sent.push({
      url,
      headers: init.headers as Record<string, string>,
      body: JSON.parse(init.body as string) as Sent["body"],
    });
    const reply = replies.shift();
    assert.ok(reply, "unexpected model request");
    return Promise.resolve(reply instanceof Response ? reply : sse(reply));
  }) as typeof fetch;
  const mcpCalls: [string, Record<string, unknown>][] = [];
  const mcp = {
    listTools: () => Promise.resolve(opts.tools ?? []),
    call: (name: string, args: Record<string, unknown>) => {
      mcpCalls.push([name, args]);
      return opts.mcpCall ? opts.mcpCall(name, args) : Promise.resolve({ ok: true });
    },
    close: () => Promise.resolve(),
  } as unknown as FerrobotMcp;
  const driver = new OpenAIDriver({
    baseUrl: new URL("http://daemon:7879/model"),
    model: "gpt-test",
    token: "agent-token",
    workspaceDir,
    stateDir,
    mcp,
    fetch: opts.fetch ?? fakeFetch,
  });
  return { driver, sent, mcpCalls, workspaceDir, stateDir };
}

function context(overrides: Partial<TurnContext> = {}): { ctx: TurnContext; events: TurnEvent[] } {
  const events: TurnEvent[] = [];
  const ctx: TurnContext = {
    turn: {
      type: "turn",
      turn_id: "t1",
      message_id: 7,
      sender: { kind: "user" },
      sender_name: "user",
      body: "hi",
    },
    instructions: "Be brief.",
    agentName: "alice",
    sessionId: null,
    signal: new AbortController().signal,
    emit: (e) => events.push(e),
    ...overrides,
  };
  return { ctx, events };
}

describe("OpenAIDriver", () => {
  it("streams text and sends model, token, tools and messages", async () => {
    const { driver, sent } = setup({ replies: [[text("Hel"), text("lo ü")]] });
    const { ctx, events } = context();
    const out = await driver.runTurn(ctx);
    assert.deepEqual(events, [
      { kind: "assistant_delta", text: "Hel" },
      { kind: "assistant_delta", text: "lo ü" },
      { kind: "assistant_text", text: "Hello ü" },
    ]);
    assert.equal(out.result, "Hello ü");
    assert.match(out.sessionId ?? "", /^[0-9a-f-]{36}$/);
    const req = at(sent, 0);
    assert.equal(req.url, "http://daemon:7879/model/chat/completions");
    assert.equal(req.headers["Authorization"], "Bearer agent-token");
    assert.equal(req.body.model, "gpt-test");
    assert.equal(req.body.stream, true);
    assert.deepEqual(
      req.body.tools.map((t) => t.function.name),
      ["bash", "read_file", "write_file"],
    );
    const system = at(req.body.messages, 0);
    assert.equal(system.role, "system");
    assert.match(system.content, /bash, read_file and write_file/);
    assert.match(system.content, /"alice"[\s\S]*Be brief\./);
    assert.deepEqual(at(req.body.messages, 1), {
      role: "user",
      content: "Message #7 from user:\n\nhi",
    });
  });

  it("runs bash in the workspace and sends the result back", async () => {
    const { driver, sent, workspaceDir } = setup({
      replies: [
        [
          call(0, { name: "bash", arguments: '{"comm' }, "call_1"),
          call(0, { arguments: 'and":"echo hi; pwd"}' }),
        ],
        [text("done")],
      ],
    });
    const { ctx, events } = context();
    const out = await driver.runTurn(ctx);
    assert.equal(out.result, "done");
    assert.deepEqual(at(events, 0), {
      kind: "tool_use",
      id: "call_1",
      name: "bash",
      input: { command: "echo hi; pwd" },
    });
    const result = at(events, 1);
    assert.ok(result.kind === "tool_result");
    assert.equal(result.id, "call_1");
    assert.equal(result.is_error, false);
    assert.match(result.output, /exit code 0[\s\S]*hi/);
    assert.ok(result.output.includes(basename(workspaceDir)), "runs in the workspace");
    assert.equal(sent.length, 2);
    const followUp = at(sent, 1).body.messages;
    assert.deepEqual(at(followUp, 2), {
      role: "assistant",
      content: "",
      tool_calls: [
        {
          id: "call_1",
          type: "function",
          function: { name: "bash", arguments: '{"command":"echo hi; pwd"}' },
        },
      ],
    });
    const tool = at(followUp, 3);
    assert.ok(tool.role === "tool");
    assert.equal(tool.tool_call_id, "call_1");
    assert.match(tool.content, /hi/);
  });

  it("adds messages that arrive mid-turn, also after the final answer", async () => {
    const { driver, sent } = setup({
      replies: [
        [call(0, { name: "bash", arguments: '{"command":"true"}' }, "call_1")],
        [text("Noted, hotels next. Flights: 3 options.")],
        [text("Trains too: 2 options.")],
      ],
    });
    const late = (id: number, body: string) => ({
      type: "steer" as const,
      turn_id: "t1",
      message_id: id,
      sender: { kind: "user" as const },
      sender_name: "user",
      body,
    });
    // Taken after the tool round, after the first answer, after the second.
    const inbox = [[late(8, "also hotels")], [late(9, "and trains")], []];
    const { ctx } = context({ takeMessages: () => inbox.shift() ?? [] });
    const out = await driver.runTurn(ctx);
    assert.equal(out.result, "Trains too: 2 options.");
    assert.equal(sent.length, 3);
    const lastUser = (i: number) =>
      at(
        at(sent, i).body.messages.filter((m) => m.role === "user"),
        -1,
      ).content;
    assert.equal(lastUser(1), "Message #8 from user, sent while you were working:\n\nalso hotels");
    assert.equal(lastUser(2), "Message #9 from user, sent while you were working:\n\nand trains");
  });

  it("writes and reads files relative to the workspace", async () => {
    const { driver, sent, workspaceDir } = setup({
      replies: [
        [
          call(0, { name: "write_file", arguments: '{"path":"notes/a.txt","content":"x"}' }, "w"),
          call(1, { name: "read_file", arguments: '{"path":"notes/a.txt"}' }, "r"),
        ],
        [text("ok")],
      ],
    });
    await driver.runTurn(context().ctx);
    assert.equal(readFileSync(join(workspaceDir, "notes/a.txt"), "utf8"), "x");
    assert.deepEqual(at(at(sent, 1).body.messages, 4), {
      role: "tool",
      tool_call_id: "r",
      content: "x",
    });
  });

  it("refuses calls the policy blocks and tells the model why", async () => {
    const { driver, sent } = setup({
      replies: [
        [
          call(0, { name: "bash", arguments: '{"command":"rm -rf /"}' }, "a"),
          call(1, { name: "read_file", arguments: '{"path":"../secret"}' }, "b"),
        ],
        [text("ok")],
      ],
    });
    const { ctx, events } = context();
    await driver.runTurn(ctx);
    assert.deepEqual(
      events
        .filter((e) => e.kind !== "assistant_delta" && e.kind !== "assistant_text")
        .map((e) => e.kind),
      ["tool_denied", "tool_denied"],
    );
    const tools = at(sent, 1).body.messages.filter((m) => m.role === "tool");
    assert.equal(at(tools, 0).content, "blocked: recursive delete of / or $HOME");
    assert.match(at(tools, 1).content, /^blocked: Read outside /);
  });

  it("offers and calls the daemon's MCP tools; tool failures do not fail the turn", async () => {
    const schema = { type: "object", properties: { to: { type: "string" } } };
    const { driver, sent, mcpCalls } = setup({
      tools: [
        { name: "send_message", description: "Send a message", inputSchema: schema },
        { name: "list_agents", inputSchema: { type: "object" } },
        { name: "recall", inputSchema: { type: "object" } },
      ],
      replies: [
        [
          call(0, { name: "mcp__ferrobot__send_message", arguments: '{"to":"bob"}' }, "s"),
          call(1, { name: "mcp__ferrobot__list_agents", arguments: "" }, "l"),
          call(2, { name: "mcp__ferrobot__recall", arguments: '{"query":"x"}' }, "r"),
        ],
        [text("sent")],
      ],
      mcpCall: (name) =>
        name === "recall"
          ? Promise.reject(new Error("recall broke"))
          : Promise.resolve({ ok: true }),
    });
    const { ctx, events } = context();
    const out = await driver.runTurn(ctx);
    assert.equal(out.result, "sent");
    assert.deepEqual(at(at(sent, 0).body.tools, 3), {
      type: "function",
      function: {
        name: "mcp__ferrobot__send_message",
        description: "Send a message",
        parameters: schema,
      },
    });
    assert.deepEqual(mcpCalls, [
      ["send_message", { to: "bob" }],
      ["list_agents", {}],
      ["recall", { query: "x" }],
    ]);
    const results = events.filter((e) => e.kind === "tool_result");
    assert.deepEqual(results, [
      { kind: "tool_result", id: "s", output: '{"ok":true}', is_error: false },
      { kind: "tool_result", id: "l", output: '{"ok":true}', is_error: false },
      { kind: "tool_result", id: "r", output: "recall broke", is_error: true },
    ]);
  });

  it("answers malformed arguments with an error and keeps the history valid", async () => {
    const { driver, sent } = setup({
      replies: [[call(0, { name: "bash", arguments: "{oops" }, "m")], [text("sorry")]],
    });
    const { ctx, events } = context();
    await driver.runTurn(ctx);
    const result = events.find((e) => e.kind === "tool_result");
    assert.ok(result?.kind === "tool_result");
    assert.equal(result.is_error, true);
    assert.match(result.output, /invalid JSON arguments for bash/);
    const assistant = at(at(sent, 1).body.messages, 2);
    assert.ok(assistant.role === "assistant");
    assert.equal(at(assistant.tool_calls ?? [], 0).function.arguments, "{}");
  });

  it("persists the session and resumes it after a restart", async () => {
    const first = setup({ replies: [[text("first")]] });
    const out = await first.driver.runTurn(context().ctx);
    const id = out.sessionId ?? "";
    assert.ok(existsSync(join(first.stateDir, `openai-${id}.json`)));

    const second = setup({ stateDir: first.stateDir, replies: [[text("second")]] });
    const resumed = await second.driver.runTurn(context({ sessionId: id }).ctx);
    assert.equal(resumed.sessionId, id);
    assert.deepEqual(
      at(second.sent, 0)
        .body.messages.slice(1)
        .map((m) => [m.role, m.content]),
      [
        ["user", "Message #7 from user:\n\nhi"],
        ["assistant", "first"],
        ["user", "Message #7 from user:\n\nhi"],
      ],
    );

    const fresh = setup({ stateDir: first.stateDir, replies: [[text("new")]] });
    const restarted = await fresh.driver.runTurn(context({ sessionId: "missing" }).ctx);
    assert.notEqual(restarted.sessionId, "missing");
    assert.equal(at(fresh.sent, 0).body.messages.length, 2);
  });

  it("kills a running command on abort", async () => {
    const abort = new AbortController();
    const { driver } = setup({
      replies: [[call(0, { name: "bash", arguments: '{"command":"sleep 30; echo late"}' }, "z")]],
    });
    const { ctx } = context({
      signal: abort.signal,
      emit: (e) => {
        if (e.kind === "tool_use")
          setTimeout(() => {
            abort.abort();
          }, 50);
      },
    });
    const started = Date.now();
    await assert.rejects(driver.runTurn(ctx), TurnAbortedError);
    assert.ok(Date.now() - started < 10_000, "did not wait for the command");
  });

  it("runs no further tools after an abort", async () => {
    const abort = new AbortController();
    const { driver } = setup({
      replies: [
        [
          call(0, { name: "write_file", arguments: '{"path":"a.txt","content":"x"}' }, "w"),
          call(1, { name: "bash", arguments: '{"command":"sleep 30"}' }, "b"),
        ],
      ],
    });
    const { ctx, events } = context({ signal: abort.signal });
    const emit = ctx.emit;
    ctx.emit = (e) => {
      emit(e);
      if (e.kind === "tool_use") abort.abort();
    };
    await assert.rejects(driver.runTurn(ctx), TurnAbortedError);
    assert.deepEqual(
      events.flatMap((e) => (e.kind === "tool_use" ? [e.name] : [])),
      ["write_file"],
    );
  });

  it("aborts a pending model request", async () => {
    const abort = new AbortController();
    const hanging = ((_url: string, init: RequestInit) =>
      new Promise<Response>((_resolve, reject) => {
        init.signal?.addEventListener("abort", () => {
          reject(new Error("aborted"));
        });
      })) as typeof fetch;
    const { driver } = setup({ fetch: hanging });
    const run = driver.runTurn(context({ signal: abort.signal }).ctx);
    setTimeout(() => {
      abort.abort();
    }, 10);
    await assert.rejects(run, TurnAbortedError);
  });

  it("reports HTTP errors with status and message", async () => {
    const envelope = JSON.stringify({
      type: "error",
      error: { type: "api_error", message: "upstream down" },
    });
    const { driver } = setup({ replies: [new Response(envelope, { status: 502 })] });
    await assert.rejects(driver.runTurn(context().ctx), /502: upstream down/);
  });

  it("fails the turn when the model never stops calling tools", async () => {
    const denied = [call(0, { name: "read_file", arguments: '{"path":"/etc/hosts"}' }, "x")];
    const { driver, sent } = setup({ replies: Array.from({ length: 60 }, () => denied) });
    await assert.rejects(driver.runTurn(context().ctx), /50 model calls/);
    assert.equal(sent.length, 50);
  });
});

describe("openai config", () => {
  const base = {
    FERROBOT_DAEMON_URL: "http://host.docker.internal:7879",
    FERROBOT_AGENT_TOKEN: "t",
    FERROBOT_DRIVER: "openai",
  };

  it("needs FERROBOT_MODEL and FERROBOT_OPENAI_BASE_URL", () => {
    const url = { FERROBOT_OPENAI_BASE_URL: "http://host.docker.internal:7879/model" };
    assert.throws(() => loadConfig({ ...base, ...url }), /FERROBOT_MODEL/);
    assert.throws(
      () => loadConfig({ ...base, FERROBOT_MODEL: "gpt-5" }),
      /FERROBOT_OPENAI_BASE_URL/,
    );
    assert.throws(
      () => loadConfig({ ...base, FERROBOT_MODEL: "gpt-5", FERROBOT_OPENAI_BASE_URL: "not a url" }),
      ConfigError,
    );
    const config = loadConfig({ ...base, ...url, FERROBOT_MODEL: "gpt-5" });
    assert.ok(config.driver === "openai");
    assert.equal(config.model, "gpt-5");
    assert.equal(config.openaiBaseUrl.href, "http://host.docker.internal:7879/model");
  });
});

describe("plainSchema", () => {
  it("inlines $defs references and drops meta keywords", () => {
    const schema = {
      $schema: "https://json-schema.org/draft/2020-12/schema",
      type: "object",
      properties: {
        card: { $ref: "#/$defs/Card" },
        tags: { type: "array", items: { $ref: "#/$defs/Tag" } },
      },
      $defs: {
        Card: { type: "object", properties: { tag: { $ref: "#/$defs/Tag" } } },
        Tag: { type: "string" },
      },
    };
    assert.deepEqual(plainSchema(schema), {
      type: "object",
      properties: {
        card: { type: "object", properties: { tag: { type: "string" } } },
        tags: { type: "array", items: { type: "string" } },
      },
    });
  });
});
