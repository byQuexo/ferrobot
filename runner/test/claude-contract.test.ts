// Fixtures are typed as the SDK's own message types, so an SDK upgrade that changes these shapes
// fails `npm run typecheck`; `query` is a fake generator (no key, no network).
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";

import type {
  Options,
  SDKAssistantMessage,
  SDKMessage,
  SDKPartialAssistantMessage,
  SDKUserMessage,
} from "@anthropic-ai/claude-agent-sdk";

import {
  ClaudeDriver,
  MAX_TOOL_OUTPUT,
  buildOptions,
  mapSdkMessage,
  systemAppend,
  type QueryFn,
} from "../src/drivers/claude.ts";
import type { TurnContext } from "../src/drivers/types.ts";
import { TurnAbortedError } from "../src/drivers/types.ts";
import type { SteerFrame, TurnEvent } from "../src/protocol.ts";
import { SessionStore } from "../src/session.ts";

const SESSION = "5f1a6a3e-2c51-4b51-9a4e-0f2b8c7d1e01";
const uuid = "8a6b4c2d-1e3f-4a5b-9c7d-0e1f2a3b4c5d" as const;

const delta: SDKPartialAssistantMessage = {
  type: "stream_event",
  event: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "Hel" } },
  parent_tool_use_id: null,
  uuid,
  session_id: SESSION,
};

const subagentDelta: SDKPartialAssistantMessage = { ...delta, parent_tool_use_id: "toolu_task" };

const assistant: SDKAssistantMessage = {
  type: "assistant",
  message: {
    id: "msg_01",
    type: "message",
    role: "assistant",
    model: "claude-test",
    content: [
      { type: "text", text: "Hello, I will ask bob.", citations: null },
      {
        type: "tool_use",
        id: "toolu_01",
        name: "mcp__ferrobot__send_message",
        input: { to: "bob", body: "hi" },
      },
    ],
    stop_reason: "tool_use",
    stop_sequence: null,
    stop_details: null,
    diagnostics: null,
    container: null,
    context_management: null,
    usage: {
      input_tokens: 1,
      output_tokens: 1,
      cache_creation_input_tokens: null,
      cache_read_input_tokens: null,
      cache_creation: null,
      server_tool_use: null,
      service_tier: null,
      inference_geo: null,
      iterations: null,
      output_tokens_details: null,
      speed: null,
      fallback_credit: null,
    },
  },
  parent_tool_use_id: null,
  uuid,
  session_id: SESSION,
};

const toolResult: SDKUserMessage = {
  type: "user",
  message: {
    role: "user",
    content: [
      {
        type: "tool_result",
        tool_use_id: "toolu_01",
        content: [{ type: "text", text: "x".repeat(MAX_TOOL_OUTPUT + 10) }],
      },
      { type: "tool_result", tool_use_id: "toolu_02", content: "boom", is_error: true },
    ],
  },
  parent_tool_use_id: null,
  session_id: SESSION,
};

// The result message has many timing fields; the driver reads only these.
const result = {
  type: "result",
  subtype: "success",
  result: "Asked bob.",
  session_id: SESSION,
} as const;
const resultMessage = result as unknown as SDKMessage;
// Compile-time check that the fields the driver reads exist with these types.
type ResultSuccess = Extract<SDKMessage, { type: "result"; subtype: "success" }>;
const _resultFields: Pick<ResultSuccess, "type" | "subtype" | "result" | "session_id"> = result;
assert.ok(_resultFields);

describe("mapSdkMessage (SDK-typed fixtures)", () => {
  it("streams top-level text deltas only", () => {
    assert.deepEqual(mapSdkMessage(delta).events, [{ kind: "assistant_delta", text: "Hel" }]);
    assert.deepEqual(mapSdkMessage(subagentDelta).events, []);
    assert.equal(mapSdkMessage(delta).sessionId, SESSION);
  });

  it("maps assistant text and tool calls with ids", () => {
    assert.deepEqual(mapSdkMessage(assistant).events, [
      { kind: "assistant_text", text: "Hello, I will ask bob." },
      {
        kind: "tool_use",
        id: "toolu_01",
        name: "mcp__ferrobot__send_message",
        input: { to: "bob", body: "hi" },
      },
    ]);
  });

  it("maps tool results, clipping long output", () => {
    const events = mapSdkMessage(toolResult).events;
    assert.equal(events.length, 2);
    const [first, second] = events;
    assert.ok(first?.kind === "tool_result" && second?.kind === "tool_result");
    assert.equal(first.id, "toolu_01");
    assert.equal(first.is_error, false);
    assert.ok(first.output.length < MAX_TOOL_OUTPUT + 100);
    assert.match(first.output, /more characters/);
    assert.deepEqual(second, {
      kind: "tool_result",
      id: "toolu_02",
      output: "boom",
      is_error: true,
    });
  });

  it("reads the final result", () => {
    assert.deepEqual(mapSdkMessage(resultMessage).outcome, { ok: true, result: "Asked bob." });
  });
});

describe("buildOptions", () => {
  it("produces SDK Options with streaming, MCP, instructions and resume", () => {
    const options: Options = buildOptions({
      ctx: { agentName: "ada", instructions: "Review pull requests." },
      workspaceDir: "/home/node/workspace",
      mcpUrl: new URL("http://host.docker.internal:7879/mcp"),
      token: "tok",
      model: "claude-test",
      resume: SESSION,
      abortController: new AbortController(),
      canUseTool: () => Promise.resolve({ behavior: "allow", updatedInput: {} }),
      audit: () => Promise.resolve({ continue: true }),
    });
    assert.equal(options.includePartialMessages, true);
    assert.equal(options.resume, SESSION);
    assert.equal(options.model, "claude-test");
    assert.deepEqual(options.settingSources, []);
    const sp = options.systemPrompt;
    assert.ok(typeof sp === "object" && !Array.isArray(sp) && sp.type === "preset");
    assert.match(sp.append ?? "", /Review pull requests\./);
    assert.match(sp.append ?? "", /"ada"/);
    assert.deepEqual(options.mcpServers?.["ferrobot"], {
      type: "http",
      url: "http://host.docker.internal:7879/mcp",
      headers: { Authorization: "Bearer tok" },
    });
    assert.equal(options.disallowedTools, undefined, "no profile: every built-in tool");
  });

  it("removes the tools of turned-off groups from the model's context", () => {
    const disallowed = (tools: string[]): string[] | undefined =>
      buildOptions({
        ctx: { agentName: "ada", instructions: "", tools },
        workspaceDir: "/w",
        mcpUrl: new URL("http://d/mcp"),
        token: "tok",
        model: undefined,
        resume: null,
        abortController: new AbortController(),
        canUseTool: () => Promise.resolve({ behavior: "allow", updatedInput: {} }),
        audit: () => Promise.resolve({ continue: true }),
      }).disallowedTools;
    assert.equal(disallowed(["team", "shell", "files", "web"]), undefined);
    const noShellNoWeb = disallowed(["team", "files"]) ?? [];
    for (const tool of ["Bash", "Monitor", "WebFetch", "WebSearch", "Agent", "Workflow"]) {
      assert.ok(noShellNoWeb.includes(tool), tool);
    }
    for (const tool of ["Read", "Write", "Edit", "mcp__ferrobot__send_message"]) {
      assert.ok(!noShellNoWeb.includes(tool), tool);
    }
    assert.ok((disallowed(["shell"]) ?? []).includes("Read"));
  });
});

describe("systemAppend", () => {
  const prompt = systemAppend("ada", "");
  for (const [what, line] of [
    ["mail wakes a sleeping teammate", /A message wakes a\s+sleeping teammate/],
    ["tasks report back on their own", /assign_task[\s\S]*answer comes back to you automatically/],
    ["agents can list their tasks", /list_tasks shows the tasks you gave and got/],
    ["delegates finish with their answer", /end your turn with your answer and it goes back/],
    ["new agents get memorable names", /scout \(research\), forge \(building\)/],
  ] as const) {
    it(`tells the agent: ${what}`, () => {
      assert.match(prompt, line);
    });
  }
});

function ctx(overrides: Partial<TurnContext> = {}): { ctx: TurnContext; events: TurnEvent[] } {
  const events: TurnEvent[] = [];
  return {
    events,
    ctx: {
      turn: {
        type: "turn",
        turn_id: "t1",
        message_id: 3,
        sender: { kind: "user" },
        sender_name: "user",
        body: "ask bob",
      },
      instructions: "be brief",
      agentName: "ada",
      sessionId: null,
      signal: new AbortController().signal,
      emit: (e) => events.push(e),
      ...overrides,
    },
  };
}

function textOf(m: SDKUserMessage | undefined): string {
  const content = m?.message.content;
  return typeof content === "string" ? content : "";
}

function driver(query: QueryFn): ClaudeDriver {
  return new ClaudeDriver({
    mcpUrl: new URL("http://daemon/mcp"),
    token: "tok",
    workspaceDir: mkdtempSync(join(tmpdir(), "ferrobot-")),
    model: undefined,
    session: new SessionStore(mkdtempSync(join(tmpdir(), "ferrobot-"))),
    query,
  });
}

describe("ClaudeDriver with a fake query()", () => {
  it("emits events in order and returns result and session", async () => {
    const prompts: string[] = [];
    const d = driver(async function* ({ prompt, options }) {
      const first: IteratorResult<SDKUserMessage> = await prompt[Symbol.asyncIterator]().next();
      prompts.push(textOf(first.done ? undefined : first.value));
      assert.equal(options.includePartialMessages, true);
      yield delta;
      yield assistant;
      yield toolResult;
      yield resultMessage;
    });
    const { ctx: c, events } = ctx();
    const out = await d.runTurn(c);
    assert.deepEqual(
      events.map((e) => e.kind),
      ["assistant_delta", "assistant_text", "tool_use", "tool_result", "tool_result"],
    );
    assert.deepEqual(out, { result: "Asked bob.", sessionId: SESSION });
    assert.match(prompts[0] ?? "", /^Message #3 from user:/);
  });

  it("aborts the SDK query on interrupt", async () => {
    const abort = new AbortController();
    let sdkAborted = false;
    const d = driver(async function* ({ options }) {
      yield delta;
      await new Promise<void>((resolve) => {
        options.abortController?.signal.addEventListener("abort", () => {
          sdkAborted = true;
          resolve();
        });
      });
      throw new Error("aborted by SDK");
    });
    const { ctx: c } = ctx({ signal: abort.signal });
    const running = d.runTurn(c);
    setTimeout(() => {
      abort.abort();
    }, 10);
    await assert.rejects(running, TurnAbortedError);
    assert.ok(sdkAborted);
  });

  it("resumes the session, and starts fresh when resume fails", async () => {
    const resumes: (string | undefined)[] = [];
    const d = driver(async function* ({ options }) {
      await Promise.resolve();
      resumes.push(options.resume);
      if (options.resume) throw new Error("No conversation found with session ID");
      yield resultMessage;
    });
    const { ctx: c } = ctx({ sessionId: "old-session" });
    const out = await d.runTurn(c);
    assert.deepEqual(resumes, ["old-session", undefined]);
    assert.equal(out.sessionId, SESSION);
  });

  it("folds messages that arrive mid-turn into the running query", async () => {
    const seen: SDKUserMessage[] = [];
    let ended = false;
    const d = driver(async function* ({ prompt }) {
      const input = prompt[Symbol.asyncIterator]();
      seen.push((await input.next()).value as SDKUserMessage);
      yield assistant;
      seen.push((await input.next()).value as SDKUserMessage);
      yield toolResult;
      yield { ...result, user_message_uuids: seen.map((m) => m.uuid) } as unknown as SDKMessage;
      ended = (await input.next()).done === true;
    });
    const late: SteerFrame[] = [
      {
        type: "steer",
        turn_id: "t1",
        message_id: 4,
        sender: { kind: "user" },
        sender_name: "user",
        body: "also book a hotel",
      },
    ];
    let calls = 0;
    const takeMessages = (): SteerFrame[] => (calls++ === 0 ? [...late] : []);
    const out = await d.runTurn(ctx({ takeMessages }).ctx);
    assert.equal(out.result, "Asked bob.");
    assert.match(
      textOf(seen[1]),
      /^Message #4 from user, sent while you were working:\n\nalso book a hotel$/,
    );
    assert.equal(seen[1]?.priority, "next");
    assert.equal(seen[1].client_composed, true, "no slash commands from message text");
    assert.ok(ended, "the input ends once every message is answered");
  });

  it("keeps the query open until a late message is answered", async () => {
    const seen: SDKUserMessage[] = [];
    const d = driver(async function* ({ prompt }) {
      const input = prompt[Symbol.asyncIterator]();
      seen.push((await input.next()).value as SDKUserMessage);
      yield { ...result, user_message_uuids: [seen[0]?.uuid] } as unknown as SDKMessage;
      // The late message came after the first answer and runs as its own turn.
      const next = await input.next();
      assert.equal(next.done, false, "input must stay open for the queued message");
      seen.push(next.value);
      yield {
        ...result,
        result: "Hotel booked.",
        user_message_uuids: [seen[1]?.uuid],
      } as unknown as SDKMessage;
    });
    let calls = 0;
    const late: SteerFrame = {
      type: "steer",
      turn_id: "t1",
      message_id: 5,
      sender: { kind: "user" },
      sender_name: "user",
      body: "hotel too",
    };
    const takeMessages = (): SteerFrame[] => (calls++ === 0 ? [late] : []);
    const out = await d.runTurn(ctx({ takeMessages }).ctx);
    assert.equal(out.result, "Hotel booked.");
    assert.equal(seen.length, 2);
  });

  it("ends even when the result does not list the turn's own prompt", async () => {
    let ended = false;
    const d = driver(async function* ({ prompt }) {
      const input = prompt[Symbol.asyncIterator]();
      await input.next();
      yield { ...result, user_message_uuids: [] } as unknown as SDKMessage;
      ended = (await input.next()).done === true;
    });
    const out = await d.runTurn(ctx().ctx);
    assert.equal(out.result, "Asked bob.");
    assert.ok(ended);
  });

  it("reports SDK error results as failures", async () => {
    const d = driver(async function* () {
      await Promise.resolve();
      yield { ...result, subtype: "error_max_turns" } as unknown as SDKMessage;
    });
    await assert.rejects(d.runTurn(ctx().ctx), /error_max_turns/);
  });
});
