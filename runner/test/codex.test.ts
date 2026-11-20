import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, describe, it } from "node:test";

import { ConfigError, loadConfig } from "../src/config.ts";
import { CodexDriver, codexConfigToml } from "../src/drivers/codex.ts";
import { TurnAbortedError, type TurnContext } from "../src/drivers/types.ts";
import type { TurnEvent } from "../src/protocol.ts";
import type { Params, Scenario, Step } from "./fixtures/fake-codex.ts";

interface Received {
  id?: string | number;
  method?: string;
  params?: Params;
  result?: Params;
  error?: { code: number; message: string };
  argv?: string[];
  cwd?: string;
  token?: string;
}

// The fake runs through a shell wrapper, like a real `codex` binary on PATH.
const BIN = join(mkdtempSync(join(tmpdir(), "ferrobot-codex-bin-")), "codex");
writeFileSync(
  BIN,
  `#!/bin/sh\nexec "${process.execPath}" --no-warnings "${join(import.meta.dirname, "fixtures", "fake-codex.ts")}" "$@"\n`,
  { mode: 0o755 },
);

const drivers: CodexDriver[] = [];
after(() => Promise.all(drivers.map((d) => d.close())));

function setup(bin = BIN) {
  const workspaceDir = mkdtempSync(join(tmpdir(), "ferrobot-ws-"));
  const stateDir = mkdtempSync(join(tmpdir(), "ferrobot-state-"));
  const codexHome = join(stateDir, "codex");
  mkdirSync(codexHome);
  const driver = new CodexDriver({
    bin,
    baseUrl: new URL("http://daemon:7879/model"),
    mcpUrl: new URL("http://daemon:7879/mcp"),
    model: "gpt-test",
    token: "agent-token",
    workspaceDir,
    stateDir,
  });
  drivers.push(driver);
  const script = (scenario: Scenario): void => {
    writeFileSync(join(codexHome, "scenario.json"), JSON.stringify(scenario));
  };
  const received = (): Received[] =>
    readFileSync(join(codexHome, "received.jsonl"), "utf8")
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line) as Received);
  const sent = (method: string): Received[] => received().filter((m) => m.method === method);
  const answer = (id: string): Received | undefined =>
    received().find((m) => m.id === id && !m.method);
  return { driver, script, received, sent, answer, workspaceDir, codexHome };
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

const message = (text: string): Params => ({ type: "agentMessage", id: "msg-1", text });
const started = (item: Params): Step => ({ notify: "item/started", params: { item } });
const completed = (item: Params): Step => ({ notify: "item/completed", params: { item } });
const turnDone = (status = "completed", error: Params | null = null): Step => ({
  notify: "turn/completed",
  params: { turn: { status, error, items: [] } },
});
const command = (
  id: string,
  cmd: string,
  status: string,
  aggregatedOutput: string | null = null,
): Params => ({
  type: "commandExecution",
  id,
  command: cmd,
  cwd: "/w",
  status,
  commandActions: [],
  aggregatedOutput,
  exitCode: null,
});
const approveCommand = (itemId: string, cmd: string): Step => ({
  request: "item/commandExecution/requestApproval",
  params: { itemId, command: cmd },
});

describe("CodexDriver", () => {
  it("does the handshake, starts a thread and streams a turn", async () => {
    const { driver, script, received, sent, workspaceDir, codexHome } = setup();
    script({
      turns: [
        [
          { notify: "turn/started", params: { turn: { status: "inProgress" } } },
          // The tail of another turn must not end this one.
          {
            notify: "turn/completed",
            params: { turnId: "turn-old", turn: { id: "turn-old", status: "failed" } },
          },
          { notify: "item/agentMessage/delta", params: { itemId: "msg-1", delta: "Hel" } },
          { notify: "item/agentMessage/delta", params: { itemId: "msg-1", delta: "lo" } },
          completed(message("Hello")),
          turnDone(),
        ],
      ],
    });
    const { ctx, events } = context();
    const out = await driver.runTurn(ctx);
    assert.deepEqual(out, { result: "Hello", sessionId: "thread-new" });
    assert.deepEqual(events, [
      { kind: "assistant_delta", text: "Hel" },
      { kind: "assistant_delta", text: "lo" },
      { kind: "assistant_text", text: "Hello" },
    ]);

    const [spawned, init, initialized, start, turn] = received();
    assert.deepEqual(spawned, {
      argv: ["app-server"],
      cwd: realpathSync(workspaceDir),
      token: "agent-token",
    });
    assert.equal(init?.method, "initialize");
    assert.equal((init.params?.["clientInfo"] as { name: string }).name, "ferrobot");
    assert.deepEqual(initialized, { method: "initialized" });
    assert.equal(start?.method, "thread/start");
    assert.equal(start.params?.["cwd"], workspaceDir);
    assert.match(String(start.params["developerInstructions"]), /"alice"[\s\S]*Be brief\./);
    assert.deepEqual(turn?.params, {
      threadId: "thread-new",
      input: [{ type: "text", text: "Message #7 from user:\n\nhi" }],
    });
    assert.equal(sent("thread/resume").length, 0);

    const config = readFileSync(join(codexHome, "config.toml"), "utf8");
    assert.equal(
      config,
      codexConfigToml({
        model: "gpt-test",
        baseUrl: new URL("http://daemon:7879/model"),
        mcpUrl: new URL("http://daemon:7879/mcp"),
      }),
    );
    assert.ok(!config.includes("agent-token"), "no secret in config.toml");
  });

  it("resumes an existing thread once per process", async () => {
    const { driver, script, sent } = setup();
    script({
      resumable: ["thread-old"],
      turns: [
        [completed(message("one")), turnDone()],
        [completed(message("two")), turnDone()],
      ],
    });
    const first = await driver.runTurn(context({ sessionId: "thread-old" }).ctx);
    const second = await driver.runTurn(context({ sessionId: "thread-old" }).ctx);
    assert.deepEqual(first, { result: "one", sessionId: "thread-old" });
    assert.deepEqual(second, { result: "two", sessionId: "thread-old" });
    const resumes = sent("thread/resume");
    assert.equal(resumes.length, 1);
    assert.equal(resumes[0]?.params?.["threadId"], "thread-old");
    assert.equal(resumes[0].params["excludeTurns"], true);
    assert.equal(sent("thread/start").length, 0);
    assert.deepEqual(
      sent("turn/start").map((m) => m.params?.["threadId"]),
      ["thread-old", "thread-old"],
    );
  });

  it("starts a new thread when the old one cannot be resumed", async () => {
    const { driver, script, sent } = setup();
    script({ turns: [[completed(message("fresh")), turnDone()]] });
    const out = await driver.runTurn(context({ sessionId: "thread-gone" }).ctx);
    assert.deepEqual(out, { result: "fresh", sessionId: "thread-new" });
    assert.equal(sent("thread/resume").length, 1);
    assert.equal(sent("thread/start").length, 1);
  });

  it("answers command approvals with the policy and maps command items", async () => {
    const { driver, script, answer } = setup();
    script({
      turns: [
        [
          started(command("c1", "ls", "inProgress")),
          approveCommand("c1", "ls"),
          completed(command("c1", "ls", "completed", "a.txt\n")),
          started(command("c2", "rm -rf /", "inProgress")),
          approveCommand("c2", "rm -rf /"),
          completed(command("c2", "rm -rf /", "declined")),
          completed(message("done")),
          turnDone(),
        ],
      ],
    });
    const { ctx, events } = context();
    await driver.runTurn(ctx);
    assert.deepEqual(events, [
      { kind: "tool_use", id: "c1", name: "Bash", input: { command: "ls" } },
      { kind: "tool_result", id: "c1", output: "a.txt\n", is_error: false },
      { kind: "tool_use", id: "c2", name: "Bash", input: { command: "rm -rf /" } },
      { kind: "tool_denied", name: "Bash", reason: "blocked: recursive delete of / or $HOME" },
      { kind: "tool_result", id: "c2", output: "declined", is_error: true },
      { kind: "assistant_text", text: "done" },
    ]);
    assert.deepEqual(answer("srv-1")?.result, { decision: "accept" });
    assert.deepEqual(answer("srv-2")?.result, { decision: "decline" });
  });

  it("checks every path of a file change and maps MCP calls", async () => {
    const { driver, script, answer, workspaceDir } = setup();
    const patch = (id: string, path: string): Params => ({
      type: "fileChange",
      id,
      status: "inProgress",
      changes: [{ path, kind: { type: "add" }, diff: "x\n" }],
    });
    const mcp = (status: string, result: Params | null): Params => ({
      type: "mcpToolCall",
      id: "m1",
      server: "ferrobot",
      tool: "send_message",
      status,
      arguments: { to: "bob" },
      result,
      error: null,
    });
    script({
      turns: [
        [
          started(patch("p1", join(workspaceDir, "ok.txt"))),
          { request: "item/fileChange/requestApproval", params: { itemId: "p1" } },
          started(patch("p2", "/etc/passwd")),
          { request: "item/fileChange/requestApproval", params: { itemId: "p2" } },
          { request: "item/fileChange/requestApproval", params: { itemId: "p-unknown" } },
          { request: "item/tool/requestUserInput", params: { itemId: "q1", questions: [] } },
          started(mcp("inProgress", null)),
          completed(mcp("completed", { content: [{ type: "text", text: "sent" }] })),
          turnDone(),
        ],
      ],
    });
    const { ctx, events } = context();
    await driver.runTurn(ctx);
    assert.deepEqual(answer("srv-1")?.result, { decision: "accept" });
    assert.deepEqual(answer("srv-2")?.result, { decision: "decline" });
    assert.deepEqual(answer("srv-3")?.result, { decision: "decline" });
    assert.equal(answer("srv-4")?.error?.code, -32601);
    assert.deepEqual(
      events.filter((e) => e.kind === "tool_denied"),
      [
        {
          kind: "tool_denied",
          name: "apply_patch",
          reason: `blocked: Write outside ${workspaceDir}`,
        },
        { kind: "tool_denied", name: "apply_patch", reason: "blocked: unknown file change" },
      ],
    );
    assert.deepEqual(events.slice(-2), [
      { kind: "tool_use", id: "m1", name: "mcp__ferrobot__send_message", input: { to: "bob" } },
      { kind: "tool_result", id: "m1", output: "sent", is_error: false },
    ]);
  });

  it("fails the turn with Codex's error", async () => {
    const { driver, script } = setup();
    script({
      turns: [
        [
          { notify: "error", params: { error: { message: "retrying" }, willRetry: true } },
          turnDone("failed", { message: "model overloaded" }),
        ],
      ],
    });
    await assert.rejects(driver.runTurn(context().ctx), /model overloaded/);
  });

  it("interrupts the turn on abort and keeps working afterwards", async () => {
    const { driver, script, sent } = setup();
    script({
      turns: [
        [
          { notify: "item/agentMessage/delta", params: { itemId: "msg-1", delta: "working" } },
          { await: "turn/interrupt" },
          turnDone("interrupted"),
        ],
        [completed(message("again")), turnDone()],
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
    assert.deepEqual(sent("turn/interrupt")[0]?.params, {
      threadId: "thread-new",
      turnId: "turn-1",
    });

    const next = await driver.runTurn(context({ sessionId: "thread-new" }).ctx);
    assert.deepEqual(next, { result: "again", sessionId: "thread-new" });
  });

  it("restarts the app-server after it exits", async () => {
    const { driver, script, sent } = setup();
    script({ turns: [[{ exit: 3 }], [completed(message("back")), turnDone()]] });
    await assert.rejects(driver.runTurn(context().ctx), /exited with code 3/);
    const out = await driver.runTurn(context().ctx);
    assert.equal(out.result, "back");
    assert.equal(sent("initialize").length, 2);
  });

  it("starts a fresh app-server after a reconfigure (new tools)", async () => {
    const { driver, script, sent } = setup();
    script({
      turns: [
        [completed(message("one")), turnDone()],
        [completed(message("two")), turnDone()],
      ],
    });
    await driver.runTurn(context().ctx);
    driver.reconfigure();
    const out = await driver.runTurn(context({ sessionId: "thread-new" }).ctx);
    assert.equal(out.result, "two");
    assert.equal(sent("initialize").length, 2);
  });

  it("says so when codex is not installed", async () => {
    const { driver } = setup(join(tmpdir(), "no-such-dir", "codex"));
    await assert.rejects(
      driver.runTurn(context().ctx),
      /codex is not installed in this sandbox image/,
    );
  });
});

describe("codexConfigToml", () => {
  it("points Codex at the proxy and the ferrobot MCP server by env var name only", () => {
    const toml = codexConfigToml({
      model: "gpt-5.5",
      baseUrl: new URL("http://host.docker.internal:7879/model/"),
      mcpUrl: new URL("http://host.docker.internal:7879/mcp"),
    });
    assert.equal(
      toml,
      [
        'model = "gpt-5.5"',
        'model_provider = "ferrobot"',
        'approval_policy = "on-request"',
        'sandbox_mode = "danger-full-access"',
        "",
        "[model_providers.ferrobot]",
        'name = "ferrobot"',
        'base_url = "http://host.docker.internal:7879/model"',
        'env_key = "FERROBOT_AGENT_TOKEN"',
        'wire_api = "responses"',
        "",
        "[mcp_servers.ferrobot]",
        'url = "http://host.docker.internal:7879/mcp"',
        'bearer_token_env_var = "FERROBOT_AGENT_TOKEN"',
        'default_tools_approval_mode = "approve"',
        "tool_timeout_sec = 600",
        "",
      ].join("\n"),
    );
  });

  it("escapes quotes in values", () => {
    const toml = codexConfigToml({
      model: 'a"b\\c',
      baseUrl: new URL("http://x/"),
      mcpUrl: new URL("http://x/mcp"),
    });
    assert.match(toml, /^model = "a\\"b\\\\c"$/m);
  });
});

describe("codex config", () => {
  const base = {
    FERROBOT_DAEMON_URL: "http://host.docker.internal:7879",
    FERROBOT_AGENT_TOKEN: "t",
    FERROBOT_DRIVER: "codex",
  };
  const url = { FERROBOT_CODEX_BASE_URL: "http://host.docker.internal:7879/model" };

  it("needs FERROBOT_MODEL and FERROBOT_CODEX_BASE_URL", () => {
    assert.throws(() => loadConfig({ ...base, ...url }), /FERROBOT_MODEL/);
    assert.throws(
      () => loadConfig({ ...base, FERROBOT_MODEL: "gpt-5.5" }),
      /FERROBOT_CODEX_BASE_URL/,
    );
    assert.throws(
      () => loadConfig({ ...base, FERROBOT_MODEL: "gpt-5.5", FERROBOT_CODEX_BASE_URL: "ftp://x" }),
      ConfigError,
    );
    const config = loadConfig({ ...base, ...url, FERROBOT_MODEL: "gpt-5.5" });
    assert.ok(config.driver === "codex");
    assert.equal(config.model, "gpt-5.5");
    assert.equal(config.codexBaseUrl.href, "http://host.docker.internal:7879/model");
    assert.equal(config.codexBin, "codex");
  });

  it("takes the binary from FERROBOT_CODEX_BIN", () => {
    const config = loadConfig({
      ...base,
      ...url,
      FERROBOT_MODEL: "m",
      FERROBOT_CODEX_BIN: "/opt/codex/bin/codex",
    });
    assert.ok(config.driver === "codex");
    assert.equal(config.codexBin, "/opt/codex/bin/codex");
  });
});
