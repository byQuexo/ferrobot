import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { MockDriver, planMockAction } from "../src/drivers/mock.ts";
import type { FerrobotMcp } from "../src/mcp-client.ts";
import type { TurnEvent } from "../src/protocol.ts";

describe("mock driver grammar", () => {
  it("routes @mentions to send_message", () => {
    assert.deepEqual(planMockAction("@bob  please say hi ", "user", "user"), {
      kind: "send",
      to: "bob",
      body: "please say hi",
    });
  });

  it("parses remember/recall", () => {
    assert.deepEqual(planMockAction("remember deploy: fridays", "user", "user"), {
      kind: "remember",
      key: "deploy",
      content: "fridays",
    });
    assert.deepEqual(planMockAction("recall deploy", "user", "user"), { kind: "recall", query: "deploy" });
  });

  it("answers agents once and never loops", () => {
    assert.deepEqual(planMockAction("hi", "agent", "alice"), { kind: "reply", to: "alice", body: "re: hi" });
    assert.deepEqual(planMockAction("re: hi", "agent", "bob"), { kind: "echo" });
    assert.deepEqual(planMockAction("hi", "user", "user"), { kind: "echo" });
  });
});

describe("MockDriver", () => {
  it("calls the MCP tool and reports events", async () => {
    const calls: [string, Record<string, unknown>][] = [];
    const fakeMcp = {
      call: (name: string, args: Record<string, unknown>) => {
        calls.push([name, args]);
        return Promise.resolve({ ok: true });
      },
      close: () => Promise.resolve(),
    } as unknown as FerrobotMcp;
    const events: TurnEvent[] = [];
    const driver = new MockDriver(fakeMcp);
    const out = await driver.runTurn({
      turn: {
        type: "turn",
        turn_id: "t1",
        message_id: 7,
        sender: { kind: "agent", id: "0b5c2a0e-8f5e-4a51-9d6e-3f1f6f0c9a11" },
        sender_name: "alice",
        body: "ping",
      },
      instructions: "",
      agentName: "bob",
      sessionId: null,
      signal: new AbortController().signal,
      emit: (e) => events.push(e),
    });
    assert.deepEqual(calls, [["send_message", { to: "alice", body: "re: ping", reply_to: 7 }]]);
    assert.equal(events[0]?.kind, "tool_use");
    assert.equal(events[1]?.kind, "assistant_text");
    assert.equal(out.sessionId, "mock-bob");
  });
});
