import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, describe, it } from "node:test";

import { ConfigError, loadConfig } from "../src/config.ts";
import { AcpDriver, selectGeminiApiKeyAuth } from "../src/drivers/acp.ts";
import { systemAppend } from "../src/drivers/claude.ts";
import { TurnAbortedError, type TurnContext } from "../src/drivers/types.ts";
import type { TurnEvent } from "../src/protocol.ts";
import type { Params, Scenario, Step } from "./fixtures/fake-acp.ts";

interface Received {
  jsonrpc?: string;
  id?: string | number;
  method?: string;
  params?: Params;
  result?: Params;
  error?: { code: number; message: string };
  argv?: string[];
  cwd?: string;
  trust?: string;
  apiKey?: string;
  baseUrl?: string;
  settings?: unknown;
}

// The daemon puts these in the sandbox env; the driver passes them through.
process.env["GEMINI_API_KEY"] = "agent-token";
process.env["GOOGLE_GEMINI_BASE_URL"] = "http://daemon:7879/model";

// The fake runs through a shell wrapper, like a real `gemini` binary on PATH.
const BIN = join(mkdtempSync(join(tmpdir(), "ferrobot-acp-bin-")), "gemini");
writeFileSync(
  BIN,
  `#!/bin/sh\nexec "${process.execPath}" --no-warnings "${join(import.meta.dirname, "fixtures", "fake-acp.ts")}" "$@"\n`,
  { mode: 0o755 },
);

const drivers: AcpDriver[] = [];
after(() => Promise.all(drivers.map((d) => d.close())));

function setup(bin = BIN) {
  const workspaceDir = mkdtempSync(join(tmpdir(), "ferrobot-ws-"));
  const homeDir = mkdtempSync(join(tmpdir(), "ferrobot-home-"));
  const driver = new AcpDriver({
    bin,
    model: "gemini-test",
    mcpUrl: new URL("http://daemon:7879/mcp"),
    token: "agent-token",
    workspaceDir,
    homeDir,
  });
  drivers.push(driver);
  const script = (scenario: Scenario): void => {
    writeFileSync(join(homeDir, "scenario.json"), JSON.stringify(scenario));
  };
  const received = (): Received[] =>
    readFileSync(join(homeDir, "received.jsonl"), "utf8")
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line) as Received);
  const sent = (method: string): Received[] => received().filter((m) => m.method === method);
  const answer = (id: string): Received | undefined => received().find((m) => m.id === id && !m.method);
  const prompts = (): string[] =>
    sent("session/prompt").map((m) => (m.params?.["prompt"] as { text: string }[])[0]?.text ?? "");
  return { driver, script, received, sent, answer, prompts, workspaceDir, homeDir };
}

function context(overrides: Partial<TurnContext> = {}): { ctx: TurnContext; events: TurnEvent[] } {
  const events: TurnEvent[] = [];
  const ctx: TurnContext = {
    turn: { type: "turn", turn_id: "t1", message_id: 7, sender: { kind: "user" }, sender_name: "user", body: "hi" },
    instructions: "Be brief.",
    agentName: "alice",
    sessionId: null,
    signal: new AbortController().signal,
    emit: (e) => events.push(e),
    ...overrides,
  };
  return { ctx, events };
}

const PROMPT = "Message #7 from user:\n\nhi";
const INSTRUCTIONS = systemAppend("alice", "Be brief.");
const chunk = (text: string, messageId?: string): Step => ({
  update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text }, ...(messageId ? { messageId } : {}) },
});
const toolCall = (toolCallId: string, kind: string, title: string, extra: Params = {}): Step => ({
  update: { sessionUpdate: "tool_call", toolCallId, kind, title, status: "pending", ...extra },
});
const toolDone = (toolCallId: string, status: string, text: string): Step => ({
  update: {
    sessionUpdate: "tool_call_update",
    toolCallId,
    status,
    content: [{ type: "content", content: { type: "text", text } }],
  },
});
// Gemini's own option set: allow_always first, so picking by position would be wrong.
const OPTIONS = [
  { optionId: "proceed_always", name: "Allow for this session", kind: "allow_always" },
  { optionId: "proceed_once", name: "Allow", kind: "allow_once" },
  { optionId: "cancel", name: "Reject", kind: "reject_once" },
];
const ask = (toolCall: Params): Step => ({
  request: "session/request_permission",
  params: { toolCall, options: OPTIONS },
});

describe("AcpDriver", () => {
  it("does the handshake, opens a session and streams a turn", async () => {
    const { driver, script, received, sent, prompts, workspaceDir, homeDir } = setup();
    script({
      turns: [
        [
          { update: { sessionUpdate: "available_commands_update", availableCommands: [] } },
          { update: { sessionUpdate: "agent_thought_chunk", content: { type: "text", text: "**Thinking**" } } },
          { update: { sessionUpdate: "plan", entries: [] } },
          chunk("Hel"),
          chunk("lo"),
          { update: { sessionUpdate: "usage_update", used: 10, size: 100 } },
          { stop: "end_turn" },
        ],
      ],
    });
    const { ctx, events } = context();
    const out = await driver.runTurn(ctx);
    assert.deepEqual(out, { result: "Hello", sessionId: "sess-new" });
    assert.deepEqual(events, [
      { kind: "assistant_delta", text: "Hel" },
      { kind: "assistant_delta", text: "lo" },
      { kind: "assistant_text", text: "Hello" },
    ]);

    const [spawned, init, auth, open, prompt] = received();
    assert.deepEqual(spawned, {
      argv: ["--acp", "--model", "gemini-test"],
      cwd: realpathSync(workspaceDir),
      trust: "true",
      apiKey: "agent-token",
      baseUrl: "http://daemon:7879/model",
      settings: { security: { auth: { selectedType: "gemini-api-key" } } },
    });
    assert.deepEqual(init, {
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: 1,
        clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
        clientInfo: { name: "ferrobot", version: "0.1.0" },
      },
    });
    assert.deepEqual(auth, { jsonrpc: "2.0", id: 2, method: "authenticate", params: { methodId: "gemini-api-key" } });
    assert.deepEqual(open, {
      jsonrpc: "2.0",
      id: 3,
      method: "session/new",
      params: {
        cwd: workspaceDir,
        mcpServers: [
          {
            type: "http",
            name: "ferrobot",
            url: "http://daemon:7879/mcp",
            headers: [{ name: "Authorization", value: "Bearer agent-token" }],
          },
        ],
      },
    });
    assert.equal(prompt?.method, "session/prompt");
    assert.equal(prompt.params?.["sessionId"], "sess-new");
    assert.deepEqual(prompts(), [`${INSTRUCTIONS}\n\n${PROMPT}`]);
    assert.equal(sent("session/load").length, 0);
    assert.ok(!readFileSync(join(homeDir, ".gemini", "settings.json"), "utf8").includes("agent-token"));
  });

  it("skips authenticate, session/load and the MCP server when the agent offers none", async () => {
    const { driver, script, sent } = setup();
    script({ agentCapabilities: {}, authMethods: [], turns: [[chunk("fresh"), { stop: "end_turn" }]] });
    const out = await driver.runTurn(context({ sessionId: "sess-old" }).ctx);
    assert.deepEqual(out, { result: "fresh", sessionId: "sess-new" });
    assert.equal(sent("authenticate").length, 0);
    assert.equal(sent("session/load").length, 0);
    assert.deepEqual(sent("session/new")[0]?.params?.["mcpServers"], []);
  });

  it("loads a saved session once, drops its replay and repeats instructions only when they change", async () => {
    const { driver, script, sent, prompts, workspaceDir } = setup();
    script({
      loadable: ["sess-old"],
      replay: [
        { sessionUpdate: "user_message_chunk", content: { type: "text", text: "old question" } },
        { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "old answer" } },
        { sessionUpdate: "tool_call", toolCallId: "old-1", title: "ls", kind: "execute", status: "completed", content: [] },
      ],
      turns: [[chunk("one")], [chunk("two")], [chunk("three")]],
    });
    const first = context({ sessionId: "sess-old" });
    assert.deepEqual(await driver.runTurn(first.ctx), { result: "one", sessionId: "sess-old" });
    assert.deepEqual(first.events, [
      { kind: "assistant_delta", text: "one" },
      { kind: "assistant_text", text: "one" },
    ]);
    assert.deepEqual(await driver.runTurn(context({ sessionId: "sess-old" }).ctx), { result: "two", sessionId: "sess-old" });
    const changed = context({ sessionId: "sess-old", instructions: "Be thorough." }).ctx;
    assert.deepEqual(await driver.runTurn(changed), { result: "three", sessionId: "sess-old" });

    const loads = sent("session/load");
    assert.equal(loads.length, 1);
    assert.equal(loads[0]?.params?.["sessionId"], "sess-old");
    assert.equal(loads[0].params["cwd"], workspaceDir);
    assert.equal((loads[0].params["mcpServers"] as unknown[]).length, 1);
    assert.equal(sent("session/new").length, 0);
    assert.deepEqual(prompts(), [
      `${INSTRUCTIONS}\n\n${PROMPT}`,
      PROMPT,
      `${systemAppend("alice", "Be thorough.")}\n\n${PROMPT}`,
    ]);
  });

  it("starts a new session when the old one cannot be loaded", async () => {
    const { driver, script, sent } = setup();
    script({ turns: [[chunk("fresh")]] });
    const out = await driver.runTurn(context({ sessionId: "sess-gone" }).ctx);
    assert.deepEqual(out, { result: "fresh", sessionId: "sess-new" });
    assert.equal(sent("session/load").length, 1);
    assert.equal(sent("session/new").length, 1);
  });

  it("answers permission requests with the policy and maps tool calls", async () => {
    const { driver, script, answer, workspaceDir } = setup();
    const ok = join(workspaceDir, "ok.txt");
    script({
      turns: [
        [
          chunk("Let me look."),
          toolCall("c1", "execute", "ls"),
          ask({ toolCallId: "c1", title: "ls", kind: "execute", status: "pending" }),
          toolDone("c1", "completed", "a.txt\n"),
          toolCall("c2", "execute", "rm -rf /"),
          // Only the id: the details come from the earlier tool_call.
          ask({ toolCallId: "c2" }),
          toolDone("c2", "failed", 'Tool "run_shell_command" was canceled by the user.'),
          toolCall("e1", "edit", "Writing to ok.txt", {
            locations: [{ path: ok }],
            content: [{ type: "diff", path: ok, oldText: null, newText: "x\n" }],
          }),
          ask({ toolCallId: "e1", kind: "edit", title: "Writing to ok.txt", locations: [{ path: ok }] }),
          {
            update: {
              sessionUpdate: "tool_call_update",
              toolCallId: "e1",
              status: "completed",
              content: [{ type: "diff", path: ok, oldText: null, newText: "x\n" }],
            },
          },
          toolCall("e2", "edit", "Writing to passwd", { locations: [{ path: "/etc/passwd" }] }),
          ask({ toolCallId: "e2", kind: "edit", title: "Writing to passwd", locations: [{ path: "/etc/passwd" }] }),
          { request: "fs/read_text_file", params: { path: "/etc/hosts" } },
          chunk("Found it."),
        ],
      ],
    });
    const { ctx, events } = context();
    const out = await driver.runTurn(ctx);
    assert.equal(out.result, "Found it.");
    assert.deepEqual(events, [
      { kind: "assistant_delta", text: "Let me look." },
      { kind: "assistant_text", text: "Let me look." },
      { kind: "tool_use", id: "c1", name: "Bash", input: { command: "ls" } },
      { kind: "tool_result", id: "c1", output: "a.txt\n", is_error: false },
      { kind: "tool_use", id: "c2", name: "Bash", input: { command: "rm -rf /" } },
      { kind: "tool_denied", name: "Bash", reason: "blocked: recursive delete of / or $HOME" },
      { kind: "tool_result", id: "c2", output: 'Tool "run_shell_command" was canceled by the user.', is_error: true },
      { kind: "tool_use", id: "e1", name: "edit", input: { title: "Writing to ok.txt" } },
      { kind: "tool_result", id: "e1", output: `${ok}\nx\n`, is_error: false },
      { kind: "tool_use", id: "e2", name: "edit", input: { title: "Writing to passwd" } },
      { kind: "tool_denied", name: "edit", reason: `blocked: Write outside ${workspaceDir}` },
      { kind: "assistant_delta", text: "Found it." },
      { kind: "assistant_text", text: "Found it." },
    ]);
    const selected = (optionId: string) => ({ outcome: { outcome: "selected", optionId } });
    assert.deepEqual(answer("srv-1")?.result, selected("proceed_once"));
    assert.deepEqual(answer("srv-2")?.result, selected("cancel"));
    assert.deepEqual(answer("srv-3")?.result, selected("proceed_once"));
    assert.deepEqual(answer("srv-4")?.result, selected("cancel"));
    assert.equal(answer("srv-5")?.error?.code, -32601);
  });

  it("starts a new message when the message id changes", async () => {
    const { driver, script } = setup();
    script({ turns: [[chunk("A", "m1"), chunk("B", "m2"), chunk("C", "m2")]] });
    const { ctx, events } = context();
    assert.equal((await driver.runTurn(ctx)).result, "BC");
    assert.deepEqual(
      events.filter((e) => e.kind === "assistant_text"),
      [
        { kind: "assistant_text", text: "A" },
        { kind: "assistant_text", text: "BC" },
      ],
    );
  });

  it("fails the turn on refusal, other stop reasons and errors", async () => {
    const { driver, script } = setup();
    script({ turns: [[{ stop: "refusal" }], [{ error: "Rate limit exceeded. Try again later." }], [{ stop: "max_tokens" }]] });
    await assert.rejects(driver.runTurn(context().ctx), /gemini ended the turn with refusal/);
    await assert.rejects(driver.runTurn(context({ sessionId: "sess-new" }).ctx), /Rate limit exceeded/);
    await assert.rejects(driver.runTurn(context({ sessionId: "sess-new" }).ctx), /max_tokens/);
  });

  it("cancels the turn on abort and keeps working afterwards", async () => {
    const { driver, script, sent } = setup();
    script({
      turns: [
        [chunk("working"), { await: "session/cancel" }, { stop: "cancelled" }],
        [chunk("again")],
      ],
    });
    const abort = new AbortController();
    const { ctx } = context({
      signal: abort.signal,
      emit: () => {
        abort.abort();
      },
    });
    await assert.rejects(driver.runTurn(ctx), TurnAbortedError);
    assert.deepEqual(sent("session/cancel"), [{ jsonrpc: "2.0", method: "session/cancel", params: { sessionId: "sess-new" } }]);

    const next = await driver.runTurn(context({ sessionId: "sess-new" }).ctx);
    assert.deepEqual(next, { result: "again", sessionId: "sess-new" });
    assert.equal(sent("initialize").length, 1);
  });

  it("restarts the agent after it exits", async () => {
    const { driver, script, sent } = setup();
    script({ turns: [[{ exit: 3 }], [chunk("back")]] });
    await assert.rejects(driver.runTurn(context().ctx), /gemini exited with code 3/);
    const out = await driver.runTurn(context().ctx);
    assert.equal(out.result, "back");
    assert.equal(sent("initialize").length, 2);
  });

  it("refuses an agent that speaks another ACP version", async () => {
    const { driver, script } = setup();
    script({ protocolVersion: 2, turns: [] });
    await assert.rejects(driver.runTurn(context().ctx), /gemini speaks ACP version 2, ferrobot speaks 1/);
  });

  it("says so when gemini is not installed", async () => {
    const { driver } = setup(join(tmpdir(), "no-such-dir", "gemini"));
    await assert.rejects(driver.runTurn(context().ctx), /gemini is not installed in this sandbox image/);
  });
});

describe("selectGeminiApiKeyAuth", () => {
  const settingsFile = (home: string): string => join(home, ".gemini", "settings.json");

  it("merges the auth type into settings.json and keeps every other key", async () => {
    const home = mkdtempSync(join(tmpdir(), "ferrobot-home-"));
    mkdirSync(join(home, ".gemini"));
    writeFileSync(
      settingsFile(home),
      JSON.stringify({ ui: { theme: "Dracula" }, security: { folderTrust: { enabled: true }, auth: { selectedType: "oauth-personal", useExternal: false } } }),
    );
    await selectGeminiApiKeyAuth(home);
    assert.deepEqual(JSON.parse(readFileSync(settingsFile(home), "utf8")), {
      ui: { theme: "Dracula" },
      security: { folderTrust: { enabled: true }, auth: { selectedType: "gemini-api-key", useExternal: false } },
    });
  });

  it("leaves a settings.json it cannot parse untouched", async () => {
    const home = mkdtempSync(join(tmpdir(), "ferrobot-home-"));
    mkdirSync(join(home, ".gemini"));
    const jsonc = '{\n  // my theme\n  "ui": { "theme": "Dracula" }\n}\n';
    writeFileSync(settingsFile(home), jsonc);
    await assert.rejects(selectGeminiApiKeyAuth(home), /cannot update .*settings\.json/);
    assert.equal(readFileSync(settingsFile(home), "utf8"), jsonc);
  });
});

describe("acp config", () => {
  const base = {
    FERROBOT_DAEMON_URL: "http://host.docker.internal:7879",
    FERROBOT_AGENT_TOKEN: "t",
    FERROBOT_DRIVER: "acp",
    FERROBOT_ACP_AGENT: "gemini",
    FERROBOT_MODEL: "gemini-2.5-pro",
    GEMINI_API_KEY: "t",
    GOOGLE_GEMINI_BASE_URL: "http://host.docker.internal:7879/model",
  };

  it("needs a supported agent, a model and the proxy env", () => {
    const without = (key: string): Record<string, string> =>
      Object.fromEntries(Object.entries(base).filter(([k]) => k !== key));
    assert.throws(() => loadConfig({ ...base, FERROBOT_ACP_AGENT: "claude-code" }), ConfigError);
    assert.throws(() => loadConfig(without("FERROBOT_ACP_AGENT")), /FERROBOT_ACP_AGENT/);
    assert.throws(() => loadConfig(without("FERROBOT_MODEL")), /FERROBOT_MODEL/);
    assert.throws(() => loadConfig(without("GEMINI_API_KEY")), /GEMINI_API_KEY/);
    assert.throws(() => loadConfig(without("GOOGLE_GEMINI_BASE_URL")), /GOOGLE_GEMINI_BASE_URL/);
    assert.throws(() => loadConfig({ ...base, GOOGLE_GEMINI_BASE_URL: "ftp://x" }), ConfigError);
    const config = loadConfig(base);
    assert.ok(config.driver === "acp");
    assert.equal(config.model, "gemini-2.5-pro");
    assert.equal(config.acpBin, "gemini");
  });

  it("takes the binary from FERROBOT_ACP_BIN", () => {
    const config = loadConfig({ ...base, FERROBOT_ACP_BIN: "/opt/gemini/bin/gemini" });
    assert.ok(config.driver === "acp");
    assert.equal(config.acpBin, "/opt/gemini/bin/gemini");
  });
});

describe("acp tool names", () => {
  it("names Gemini's ferrobot MCP calls like the other drivers", async () => {
    const { toolUse } = await import("../src/drivers/acp.ts");
    assert.equal(
      toolUse({ toolCallId: "mcp_ferrobot_remember__mcp_ferrobot_remember_1_0", kind: "other", title: "remember(...)" }).name,
      "mcp__ferrobot__remember",
    );
    assert.equal(toolUse({ toolCallId: "x_1", kind: "read", title: "Read a" }).name, "read");
  });
});
