import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { MockDriver, planMockAction, streamPieces, type MockAction } from "../src/drivers/mock.ts";
import type { FerrobotMcp } from "../src/mcp-client.ts";
import type { TurnEvent } from "../src/protocol.ts";

describe("mock driver grammar", () => {
  it("plans the action for each message", () => {
    const cases: [string, Parameters<typeof planMockAction>, MockAction][] = [
      [
        "@mention",
        ["@bob  please say hi ", "user", "user"],
        { kind: "send", to: "bob", body: "please say hi" },
      ],
      [
        "remember",
        ["remember deploy: fridays", "user", "user"],
        { kind: "remember", key: "deploy", content: "fridays" },
      ],
      ["recall", ["recall deploy", "user", "user"], { kind: "recall", query: "deploy" }],
      [
        "agent gets one reply",
        ["hi", "agent", "alice"],
        { kind: "reply", to: "alice", body: "re: hi" },
      ],
      ["a reply is not answered (no loop)", ["re: hi", "agent", "bob"], { kind: "echo" }],
      ["user echo", ["hi", "user", "user"], { kind: "echo" }],
      ["routine echo", ["daily digest", "routine", "routine:digest"], { kind: "echo" }],
      [
        "question cards",
        ["ask Which day?: Thu | Fri | ...; Where+: email | chat; Notes", "user", "user"],
        {
          kind: "ask",
          questions: [
            {
              prompt: "Which day?",
              kind: "single",
              choices: [{ label: "Thu" }, { label: "Fri" }],
              allow_custom: true,
            },
            {
              prompt: "Where",
              kind: "multi",
              choices: [{ label: "email" }, { label: "chat" }],
              allow_custom: false,
            },
            { prompt: "Notes", kind: "text", choices: [], allow_custom: false },
          ],
        },
      ],
    ];
    for (const [label, args, expected] of cases)
      assert.deepEqual(planMockAction(...args), expected, label);
  });

  it("splits streaming pieces losslessly", () => {
    assert.deepEqual(streamPieces("a  b\nc"), ["a  ", "b\n", "c"]);
    assert.deepEqual(streamPieces(""), []);
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
    assert.deepEqual(calls, [
      [
        "send_message",
        { to: "0b5c2a0e-8f5e-4a51-9d6e-3f1f6f0c9a11", body: "re: ping", reply_to: 7 },
      ],
    ]);
    const kinds = events.map((e) => e.kind);
    assert.deepEqual(kinds.slice(0, 2), ["tool_use", "tool_result"]);
    assert.equal(kinds.at(-1), "assistant_text");
    assert.ok(kinds.includes("assistant_delta"));
    const [use, result] = events;
    assert.ok(use?.kind === "tool_use" && result?.kind === "tool_result");
    assert.equal(use.id, result.id, "result pairs with its call");
    const streamed = events.flatMap((e) => (e.kind === "assistant_delta" ? [e.text] : [])).join("");
    const final = events.at(-1);
    assert.ok(final?.kind === "assistant_text");
    assert.equal(streamed, final.text);
    assert.equal(out.sessionId, "mock-bob");
  });
});
