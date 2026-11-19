import assert from "node:assert/strict";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";

import { ConfigError, loadConfig, mcpUrl, runnerWsUrl } from "../src/config.ts";
import { TurnAbortedError, type ModelDriver } from "../src/drivers/types.ts";
import { backoffDelay } from "../src/link.ts";
import type { RunnerFrame } from "../src/protocol.ts";
import { Runner } from "../src/runner.ts";
import { SessionStore } from "../src/session.ts";

describe("config", () => {
  const base = { FERROBOT_DAEMON_URL: "http://host.docker.internal:7879", FERROBOT_AGENT_TOKEN: "t", HOME: "/home/node" };

  it("loads defaults and derives URLs", () => {
    const c = loadConfig(base);
    assert.equal(c.driver, "mock");
    assert.equal(c.workspaceDir, "/home/node/workspace");
    assert.equal(runnerWsUrl(c.daemonUrl).toString(), "ws://host.docker.internal:7879/runner/ws");
    assert.equal(mcpUrl(c.daemonUrl).toString(), "http://host.docker.internal:7879/mcp");
  });

  it("rejects bad input", () => {
    assert.throws(() => loadConfig({}), ConfigError);
    assert.throws(() => loadConfig({ ...base, FERROBOT_DAEMON_URL: "ftp://x" }), ConfigError);
    assert.throws(() => loadConfig({ ...base, FERROBOT_DRIVER: "gpt" }), ConfigError);
    assert.throws(() => loadConfig({ ...base, FERROBOT_DRIVER: "claude" }), /ANTHROPIC_API_KEY/);
  });
});

describe("backoff", () => {
  it("grows exponentially, is capped and jittered", () => {
    const opts = { initialMs: 500, maxMs: 30_000 };
    assert.equal(backoffDelay(0, opts, () => 1), 500);
    assert.equal(backoffDelay(3, opts, () => 1), 4000);
    assert.equal(backoffDelay(30, opts, () => 1), 30_000);
    assert.equal(backoffDelay(30, opts, () => 0), 15_000);
  });
});

describe("session store", () => {
  it("roundtrips atomically", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ferrobot-"));
    const s = new SessionStore(dir);
    assert.equal((await s.load()).sessionId, null);
    await s.save("abc");
    assert.equal((await s.load()).sessionId, "abc");
    await s.audit({ tool: "Bash" });
    assert.match(readFileSync(join(dir, "audit.jsonl"), "utf8"), /"tool":"Bash"/);
  });
});

function turn(id: string) {
  return {
    type: "turn" as const,
    turn_id: id,
    message_id: 1,
    sender: { kind: "user" as const },
    sender_name: "user",
    body: "hi",
  };
}

describe("Runner", () => {
  it("reports completion and persists the session", async () => {
    const sent: RunnerFrame[] = [];
    const dir = mkdtempSync(join(tmpdir(), "ferrobot-"));
    const driver: ModelDriver = {
      name: "fake",
      runTurn: (ctx) => {
        ctx.emit({ kind: "assistant_text", text: "hello" });
        return Promise.resolve({ result: "hello", sessionId: "s-1" });
      },
      close: () => Promise.resolve(),
    };
    const session = new SessionStore(dir);
    const runner = new Runner({ driver, session, agentName: "a", send: (f) => sent.push(f) > 0 }, null);
    runner.handle(turn("t1"));
    await waitFor(() => sent.some((f) => f.type === "turn_done"));
    const done = sent.find((f) => f.type === "turn_done");
    assert.deepEqual(done && { o: done.outcome, s: done.session_id }, { o: "completed", s: "s-1" });
    assert.equal((await session.load()).sessionId, "s-1");
  });

  it("interrupts the matching turn", async () => {
    const sent: RunnerFrame[] = [];
    const driver: ModelDriver = {
      name: "slow",
      runTurn: (ctx) =>
        new Promise((_resolve, reject) => {
          ctx.signal.addEventListener("abort", () => { reject(new TurnAbortedError("x")); });
        }),
      close: () => Promise.resolve(),
    };
    const session = new SessionStore(mkdtempSync(join(tmpdir(), "ferrobot-")));
    const runner = new Runner({ driver, session, agentName: "a", send: (f) => sent.push(f) > 0 }, null);
    runner.handle(turn("t1"));
    assert.equal(runner.busy, true);
    runner.handle({ type: "interrupt", turn_id: "other" });
    assert.equal(runner.busy, true);
    runner.handle({ type: "interrupt", turn_id: "t1" });
    await waitFor(() => sent.some((f) => f.type === "turn_done"));
    const done = sent.find((f) => f.type === "turn_done");
    assert.equal(done?.type === "turn_done" ? done.outcome : null, "interrupted");
    assert.equal(runner.busy, false);
  });
});

async function waitFor(cond: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (!cond()) {
    if (Date.now() - start > timeoutMs) throw new Error("timeout");
    await new Promise((r) => setTimeout(r, 10));
  }
}

describe("Runner configure", () => {
  it("applies a new name and instructions from the next turn", async () => {
    const seen: string[] = [];
    const driver: ModelDriver = {
      name: "fake",
      runTurn: (ctx) => {
        seen.push(`${ctx.agentName}:${ctx.instructions}`);
        return Promise.resolve({ result: null, sessionId: null });
      },
      close: () => Promise.resolve(),
    };
    const dir = mkdtempSync(join(tmpdir(), "ferrobot-"));
    const sent: RunnerFrame[] = [];
    const runner = new Runner(
      { driver, session: new SessionStore(dir), agentName: "a", send: (f) => sent.push(f) > 0 },
      null,
    );
    runner.handle({
      type: "welcome",
      protocol_version: 1,
      agent_id: "0b5c2a0e-8f5e-4a51-9d6e-3f1f6f0c9a11",
      agent_name: "a",
      instructions: "old",
      resume_session_id: null,
    });
    runner.handle(turn("t1"));
    await new Promise((r) => setTimeout(r, 10));
    runner.handle({ type: "configure", agent_name: "b", instructions: "new" });
    runner.handle(turn("t2"));
    await new Promise((r) => setTimeout(r, 10));
    assert.deepEqual(seen, ["a:old", "b:new"]);
  });
});

