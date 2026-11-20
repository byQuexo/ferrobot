import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { DaemonClient, type MailMessage, type StoredEvent } from "../src/api.ts";
import { hash } from "../src/lib/avatar.ts";
import { characterOf, deriveCharacter, moodOf, rerollCharacter } from "../src/lib/character.ts";
import { relativeTime, toolLabel } from "../src/lib/format.ts";
import { buildTimeline } from "../src/lib/timeline.ts";

const ME = "11111111-1111-4111-8111-111111111111";
const BOB = "22222222-2222-4222-8222-222222222222";

function msg(id: number, sender: MailMessage["sender"], recipient: string, body: string, at: string): MailMessage {
  return { id, sender, recipient, body, status: "delivered", attempts: 1, created_at: at };
}

function ev(id: number, kind: string, payload: Record<string, unknown>, at: string, turn = "t1"): StoredEvent {
  return { id, agent_id: ME, turn_id: turn, kind, payload: payload as StoredEvent["payload"], created_at: at };
}

const names = { agents: new Map([[ME, "ada"], [BOB, "bob"]]), routines: new Map([[4, "digest"]]) };

describe("avatar", () => {
  it("derives a stable character look from the id", () => {
    assert.equal(hash("ada"), hash("ada"));
    assert.notEqual(hash("ada"), hash("bob"));
    assert.deepEqual(deriveCharacter(ME), deriveCharacter(ME));
    const looks = new Set(Array.from({ length: 40 }, (_, i) => JSON.stringify(deriveCharacter(`agent-${String(i)}`))));
    assert.ok(looks.size > 20, "looks should vary");
  });
  it("prefers the stored look and re-rolls to a different one", () => {
    const stored = { body: "bean", eyes: "visor", hue: 200 } as const;
    assert.deepEqual(characterOf({ id: ME, name: "ada", character: stored }), stored);
    assert.deepEqual(characterOf({ id: ME, name: "ada", character: null }), deriveCharacter(ME));
    assert.notDeepEqual(rerollCharacter(stored), stored);
  });
  it("maps presence to a mood", () => {
    assert.equal(moodOf("working"), "working");
    assert.equal(moodOf("starting"), "thinking");
    assert.equal(moodOf("sleeping"), "sleeping");
    assert.equal(moodOf("awake"), "idle");
    assert.equal(moodOf(undefined), "idle");
  });
});

describe("format", () => {
  it("relative times", () => {
    const now = Date.parse("2026-01-01T12:00:00Z");
    assert.equal(relativeTime("2026-01-01T11:59:50Z", now), "just now");
    assert.equal(relativeTime("2026-01-01T11:55:00Z", now), "5 min ago");
    assert.equal(relativeTime("2026-01-01T15:00:00Z", now), "in 3 h");
  });
  it("splits MCP tool names", () => {
    assert.deepEqual(toolLabel("mcp__ferrobot__send_message"), { server: "ferrobot", tool: "send_message" });
    assert.deepEqual(toolLabel("Bash"), { server: null, tool: "Bash" });
  });
});

describe("buildTimeline", () => {
  it("orders messages and events and pairs tool results", () => {
    const items = buildTimeline(
      ME,
      [
        msg(1, { kind: "user" }, ME, "ask bob", "2026-01-01T10:00:00Z"),
        msg(2, { kind: "agent", id: ME }, BOB, "hi bob", "2026-01-01T10:00:02Z"),
        msg(3, { kind: "agent", id: BOB }, ME, "re: hi bob", "2026-01-01T10:00:05Z"),
        msg(4, { kind: "routine", id: 4 }, ME, "daily digest", "2026-01-01T10:00:09Z"),
      ],
      [
        ev(10, "message_in", { message_id: 1 }, "2026-01-01T10:00:01Z"),
        ev(11, "tool_use", { id: "c1", name: "mcp__ferrobot__send_message", input: { to: "bob" } }, "2026-01-01T10:00:01.500Z"),
        ev(12, "tool_result", { id: "c1", output: "sent", is_error: false }, "2026-01-01T10:00:02Z"),
        ev(13, "assistant_text", { text: "Asked bob." }, "2026-01-01T10:00:03Z"),
        ev(14, "turn_done", { outcome: "completed" }, "2026-01-01T10:00:03Z"),
        ev(15, "sleeping", { reason: "idle" }, "2026-01-01T10:00:08Z", ""),
      ],
      new Map(),
      names,
    );
    assert.deepEqual(
      items.map((i) => i.type),
      ["user", "tool", "outgoing", "assistant", "incoming", "system", "routine"],
    );
    const tool = items[1];
    assert.ok(tool?.type === "tool");
    assert.equal(tool.output, "sent");
    const incoming = items[4];
    assert.ok(incoming?.type === "incoming" && incoming.from === "bob");
    const routine = items[6];
    assert.ok(routine?.type === "routine" && routine.routine === "digest");
  });

  it("shows one notice when sleep deepens", () => {
    const items = buildTimeline(
      ME,
      [],
      [
        ev(1, "sleeping", { reason: "idle", level: "warm" }, "2026-01-01T12:27:00Z", ""),
        ev(2, "runner_disconnected", {}, "2026-01-01T12:37:00Z", ""),
        ev(3, "sleeping", { reason: "idle", level: "cold" }, "2026-01-01T12:37:00Z", ""),
        ev(4, "awake", {}, "2026-01-01T13:00:00Z", ""),
        ev(5, "sleeping", { reason: "idle", level: "warm" }, "2026-01-01T13:05:00Z", ""),
      ],
      new Map(),
      names,
    );
    assert.deepEqual(
      items.map((i) => (i.type === "system" ? [i.text, i.at] : null)),
      [
        ["Went to sleep after being idle", "2026-01-01T12:27:00Z"],
        ["Woke up", "2026-01-01T13:00:00Z"],
        ["Went to sleep after being idle", "2026-01-01T13:05:00Z"],
      ],
    );
  });

  it("shows live text until the final text arrives", () => {
    const live = new Map([["t1", "Hel"], ["t2", "Wor"]]);
    const items = buildTimeline(ME, [], [ev(1, "assistant_text", { text: "Hello" }, "2026-01-01T10:00:00Z", "t1")], live, names);
    assert.deepEqual(
      items.map((i) => (i.type === "assistant" ? [i.text, i.streaming] : null)),
      [["Hello", false], ["Wor", true]],
    );
  });

  it("surfaces failures and denials", () => {
    const items = buildTimeline(
      ME,
      [],
      [
        ev(1, "tool_denied", { name: "Bash", reason: "outside workspace" }, "2026-01-01T10:00:00Z"),
        ev(2, "turn_done", { outcome: "failed", error: "model unavailable" }, "2026-01-01T10:00:01Z"),
      ],
      new Map(),
      names,
    );
    assert.ok(items[0]?.type === "tool" && items[0].denied === "outside workspace");
    assert.ok(items[1]?.type === "error" && items[1].text === "model unavailable");
  });
});

describe("DaemonClient", () => {
  it("puts the token in URLs that cannot carry headers", () => {
    const c = new DaemonClient("http://127.0.0.1:7878/", "tok-123");
    assert.equal(c.url("/v1/agents/a/avatar?v=2"), "http://127.0.0.1:7878/v1/agents/a/avatar?v=2&access_token=tok-123");
    assert.equal(c.url("/v1/ws"), "http://127.0.0.1:7878/v1/ws?access_token=tok-123");
    assert.equal(new DaemonClient("http://h").url("/v1/ws"), "http://h/v1/ws");
  });
});

describe("mentions", () => {
  it("splits id tokens and renders current names as text", async () => {
    const { mentionsAsText, splitMentions } = await import("../src/lib/mentions.ts");
    const text = `hi <@${BOB}>, see <@not-an-id>`;
    assert.deepEqual(splitMentions(text), [{ text: "hi " }, { mention: BOB }, { text: ", see <@not-an-id>" }]);
    assert.equal(mentionsAsText(text, (id) => (id === BOB ? "robert" : undefined)), "hi @robert, see <@not-an-id>");
    assert.equal(mentionsAsText(`<@${ME}>`, () => undefined), "@unknown-agent");
  });
});

describe("groupExchanges", () => {
  it("folds consecutive agent mail with one peer and its send_message calls", async () => {
    const { groupExchanges } = await import("../src/lib/timeline.ts");
    const items = buildTimeline(
      ME,
      [
        msg(1, { kind: "user" }, ME, "ask bob", "2026-01-01T10:00:00Z"),
        msg(2, { kind: "agent", id: ME }, BOB, "hi bob", "2026-01-01T10:00:02Z"),
        msg(3, { kind: "agent", id: BOB }, ME, "re: hi bob", "2026-01-01T10:00:05Z"),
      ],
      [
        ev(11, "tool_use", { id: "c1", name: "mcp__ferrobot__send_message", input: { to: "bob" } }, "2026-01-01T10:00:01Z"),
        ev(12, "tool_use", { id: "c2", name: "mcp__ferrobot__recall", input: {} }, "2026-01-01T10:00:06Z"),
        ev(13, "assistant_text", { text: "done" }, "2026-01-01T10:00:07Z"),
      ],
      new Map(),
      names,
    );
    const shown = groupExchanges(items);
    assert.deepEqual(shown.map((i) => i.type), ["user", "exchange", "tool", "assistant"]);
    const x = shown[1];
    assert.ok(x?.type === "exchange");
    assert.equal(x.peerId, BOB);
    assert.equal(x.messages.length, 2);
    assert.equal(x.tools.length, 1);
  });
});

describe("question cards", () => {
  it("shows the card instead of the ask_user call and hides the answer message", () => {
    const card = { title: null, questions: [{ id: "q1", prompt: "Ship?", kind: "single", allow_custom: false,
      choices: [{ label: "Yes", value: "yes" }, { label: "No", value: "no" }] }] };
    const items = buildTimeline(
      ME,
      [msg(5, { kind: "user" }, ME, "Answers to your questions:\n- Ship? Yes", "2026-01-01T10:00:09Z")],
      [
        ev(1, "tool_use", { id: "c1", name: "mcp__ferrobot__ask_user", input: card }, "2026-01-01T10:00:00Z"),
        ev(2, "question", { version: 1, card }, "2026-01-01T10:00:01Z"),
        ev(3, "tool_result", { id: "c1", output: "{}", is_error: false }, "2026-01-01T10:00:02Z"),
        ev(4, "question_answered", { question: 2, message_id: 5, answers: [{ question_id: "q1", values: ["yes"] }] },
          "2026-01-01T10:00:09Z"),
      ],
      new Map(),
      names,
    );
    assert.deepEqual(items.map((i) => i.type), ["question"]);
    const q = items[0];
    assert.ok(q?.type === "question" && q.answered?.message_id === 5);
  });
});
