import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { DaemonClient, type MailMessage, type StoredEvent } from "../src/api.ts";
import { hash } from "../src/lib/avatar.ts";
import { characterOf, deriveCharacter, moodOf, rerollCharacter } from "../src/lib/character.ts";
import { relativeTime, stepLabel, toolLabel } from "../src/lib/format.ts";
import { buildTimeline, sharedFiles } from "../src/lib/timeline.ts";
import { parseBlocks, parseInline } from "../src/lib/markdown.ts";
import { modelOptions, withSaved } from "../src/lib/models.ts";
import { moveId, orderAgents } from "../src/lib/order.ts";
import { liveEvent, pcm16Base64, speakable } from "../src/lib/voice.ts";
import {
  chartMetric,
  costLabel,
  niceTicks,
  rangeOf,
  sortGroups,
  stackDays,
  tickLabel,
  toCsv,
  zoneDays,
} from "../src/lib/usage.ts";
import type { UsageGroup } from "../src/api.ts";

const ME = "11111111-1111-4111-8111-111111111111";
const BOB = "22222222-2222-4222-8222-222222222222";

function msg(
  id: number,
  sender: MailMessage["sender"],
  recipient: string,
  body: string,
  at: string,
): MailMessage {
  return { id, sender, recipient, body, status: "delivered", attempts: 1, created_at: at };
}

function ev(
  id: number,
  kind: string,
  payload: Record<string, unknown>,
  at: string,
  turn = "t1",
): StoredEvent {
  return {
    id,
    agent_id: ME,
    turn_id: turn,
    kind,
    payload: payload as StoredEvent["payload"],
    created_at: at,
  };
}

const names = {
  agents: new Map([
    [ME, "ada"],
    [BOB, "bob"],
  ]),
  routines: new Map([[4, "digest"]]),
};

describe("avatar", () => {
  it("derives a stable character look from the id", () => {
    assert.equal(hash("ada"), hash("ada"));
    assert.notEqual(hash("ada"), hash("bob"));
    assert.deepEqual(deriveCharacter(ME), deriveCharacter(ME));
    const looks = new Set(
      Array.from({ length: 40 }, (_, i) => JSON.stringify(deriveCharacter(`agent-${String(i)}`))),
    );
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
  it("says what a running tool is doing", () => {
    const nameOf = (id: string): string | undefined => (id === "b-1" ? "bob" : undefined);
    assert.equal(stepLabel("mcp__ferrobot__send_message", { to: "b-1" }, nameOf), "Messaging @bob");
    assert.equal(stepLabel("mcp__ferrobot__github__create_issue", {}, nameOf), "Using github");
    assert.equal(stepLabel("Bash", { command: "ls -la" }, nameOf), "Working on its computer");
    assert.equal(
      stepLabel("bash", { command: "curl x", description: "checking flight prices" }, nameOf),
      "Checking flight prices",
    );
    assert.equal(stepLabel("Read", { file_path: "/w/notes/plan.md" }, nameOf), "Reading plan.md");
    assert.equal(
      stepLabel("WebFetch", { url: "https://example.com/x" }, nameOf),
      "Reading example.com",
    );
    assert.equal(stepLabel("mcp__other__thing", {}, nameOf), "Using other");
  });

  it("splits MCP tool names", () => {
    assert.deepEqual(toolLabel("mcp__ferrobot__send_message"), {
      server: "ferrobot",
      tool: "send_message",
    });
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
        msg(5, { kind: "routine", id: 9 }, ME, "from a deleted routine", "2026-01-01T10:00:10Z"),
      ],
      [
        ev(10, "message_in", { message_id: 1 }, "2026-01-01T10:00:01Z"),
        ev(
          11,
          "tool_use",
          { id: "c1", name: "mcp__ferrobot__send_message", input: { to: "bob" } },
          "2026-01-01T10:00:01.500Z",
        ),
        ev(
          12,
          "tool_result",
          { id: "c1", output: "sent", is_error: false },
          "2026-01-01T10:00:02Z",
        ),
        ev(13, "assistant_text", { text: "Asked bob." }, "2026-01-01T10:00:03Z"),
        ev(14, "turn_done", { outcome: "completed" }, "2026-01-01T10:00:03Z"),
        ev(15, "sleeping", { reason: "idle" }, "2026-01-01T10:00:08Z", ""),
      ],
      new Map(),
      names,
    );
    assert.deepEqual(
      items.map((i) => i.type),
      ["user", "tool", "outgoing", "assistant", "incoming", "system", "routine", "routine"],
    );
    const tool = items[1];
    assert.ok(tool?.type === "tool");
    assert.equal(tool.output, "sent");
    const incoming = items[4];
    assert.ok(incoming?.type === "incoming" && incoming.from === "bob");
    const routine = items[6];
    assert.ok(routine?.type === "routine" && routine.routine === "digest");
    const deleted = items[7];
    assert.ok(deleted?.type === "routine" && deleted.routine === null);
  });

  it("shows shared files and finds them by name", () => {
    const events = [
      ev(1, "file", { name: "plan.md", size: 2048 }, "2026-01-01T12:00:00Z", "t1"),
      ev(2, "file", { name: "plan.md", size: 4096 }, "2026-01-01T12:05:00Z", "t2"),
    ];
    const items = buildTimeline(ME, [], events, new Map(), names);
    assert.deepEqual(
      items.map((i) => (i.type === "file" ? [i.id, i.name, i.size] : null)),
      [
        [1, "plan.md", 2048],
        [2, "plan.md", 4096],
      ],
    );
    assert.equal(sharedFiles(events).get("plan.md"), 2, "the latest version wins");
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

  it("notes agents created in auto mode", () => {
    const created = { name: "scout", id: BOB, by: "ada", auto: true };
    const items = buildTimeline(
      ME,
      [],
      [ev(1, "agent_created", created, "2026-01-01T12:00:00Z", "")],
      new Map(),
      names,
    );
    assert.deepEqual(
      items.map((i) => (i.type === "system" ? i.text : null)),
      ["ada created scout (auto mode)"],
    );
  });

  it("shows live text until the final text arrives", () => {
    const live = new Map([
      ["t1", "Hel"],
      ["t2", "Wor"],
    ]);
    const items = buildTimeline(
      ME,
      [],
      [ev(1, "assistant_text", { text: "Hello" }, "2026-01-01T10:00:00Z", "t1")],
      live,
      names,
    );
    assert.deepEqual(
      items.map((i) => (i.type === "assistant" ? [i.text, i.streaming] : null)),
      [
        ["Hello", false],
        ["Wor", true],
      ],
    );
  });

  it("surfaces failures and denials", () => {
    const items = buildTimeline(
      ME,
      [],
      [
        ev(1, "tool_denied", { name: "Bash", reason: "outside workspace" }, "2026-01-01T10:00:00Z"),
        ev(
          2,
          "turn_done",
          { outcome: "failed", error: "model unavailable" },
          "2026-01-01T10:00:01Z",
        ),
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
    assert.equal(
      c.url("/v1/agents/a/avatar?v=2"),
      "http://127.0.0.1:7878/v1/agents/a/avatar?v=2&access_token=tok-123",
    );
    assert.equal(c.url("/v1/ws"), "http://127.0.0.1:7878/v1/ws?access_token=tok-123");
    assert.equal(new DaemonClient("http://h").url("/v1/ws"), "http://h/v1/ws");
  });
});

describe("mentions", () => {
  it("splits id tokens and renders current names as text", async () => {
    const { mentionsAsText, splitMentions } = await import("../src/lib/mentions.ts");
    const text = `hi <@${BOB}>, see <@not-an-id>`;
    assert.deepEqual(splitMentions(text), [
      { text: "hi " },
      { mention: BOB },
      { text: ", see <@not-an-id>" },
    ]);
    assert.equal(
      mentionsAsText(text, (id) => (id === BOB ? "robert" : undefined)),
      "hi @robert, see <@not-an-id>",
    );
    assert.equal(
      mentionsAsText(`<@${ME}>`, () => undefined),
      "@unknown-agent",
    );
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
        ev(
          11,
          "tool_use",
          { id: "c1", name: "mcp__ferrobot__send_message", input: { to: "bob" } },
          "2026-01-01T10:00:01Z",
        ),
        ev(
          12,
          "tool_use",
          { id: "c2", name: "mcp__ferrobot__recall", input: {} },
          "2026-01-01T10:00:06Z",
        ),
        ev(13, "assistant_text", { text: "done" }, "2026-01-01T10:00:07Z"),
      ],
      new Map(),
      names,
    );
    const shown = groupExchanges(items);
    assert.deepEqual(
      shown.map((i) => i.type),
      ["user", "exchange", "tool", "assistant"],
    );
    const x = shown[1];
    assert.ok(x?.type === "exchange");
    assert.equal(x.peerId, BOB);
    assert.equal(x.messages.length, 2);
    assert.equal(x.tools.length, 1);
  });
});

describe("question cards", () => {
  it("shows the card instead of the ask_user call and hides the answer message", () => {
    const card = {
      title: null,
      questions: [
        {
          id: "q1",
          prompt: "Ship?",
          kind: "single",
          allow_custom: false,
          choices: [
            { label: "Yes", value: "yes" },
            { label: "No", value: "no" },
          ],
        },
      ],
    };
    const items = buildTimeline(
      ME,
      [
        msg(
          5,
          { kind: "user" },
          ME,
          "Answers to your questions:\n- Ship? Yes",
          "2026-01-01T10:00:09Z",
        ),
      ],
      [
        ev(
          1,
          "tool_use",
          { id: "c1", name: "mcp__ferrobot__ask_user", input: card },
          "2026-01-01T10:00:00Z",
        ),
        ev(2, "question", { version: 1, card }, "2026-01-01T10:00:01Z"),
        ev(3, "tool_result", { id: "c1", output: "{}", is_error: false }, "2026-01-01T10:00:02Z"),
        ev(
          4,
          "question_answered",
          { question: 2, message_id: 5, answers: [{ question_id: "q1", values: ["yes"] }] },
          "2026-01-01T10:00:09Z",
        ),
      ],
      new Map(),
      names,
    );
    assert.deepEqual(
      items.map((i) => i.type),
      ["question"],
    );
    const q = items[0];
    assert.ok(q?.type === "question" && q.answered?.message_id === 5);
  });
});

describe("markdown", () => {
  it("splits blocks", () => {
    const md =
      "## Plan\n\n- **Day 1**: Gothic Quarter\n  - Boqueria\n1. one\n\n> note\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\nx = 1\n```\n---\nend\nline 2";
    assert.deepEqual(parseBlocks(md), [
      { kind: "heading", level: 2, text: "Plan" },
      {
        kind: "list",
        ordered: false,
        items: [
          { text: "**Day 1**: Gothic Quarter", depth: 0 },
          { text: "Boqueria", depth: 1 },
        ],
      },
      { kind: "list", ordered: true, items: [{ text: "one", depth: 0 }] },
      { kind: "quote", text: "note" },
      { kind: "table", head: ["a", "b"], rows: [["1", "2"]] },
      { kind: "code", text: "x = 1" },
      { kind: "hr" },
      { kind: "para", text: "end\nline 2" },
    ]);
  });

  it("parses inline marks, links and mentions without touching code", () => {
    const id = "11111111-2222-4333-8444-555555555555";
    assert.deepEqual(
      parseInline(
        `**bold _it_** see [docs](https://x.dev/a) or https://y.dev. <@${id}> \`**no**\` snake_case_name`,
      ),
      [
        { strong: [{ text: "bold " }, { em: [{ text: "it" }] }] },
        { text: " see " },
        { link: "https://x.dev/a", children: [{ text: "docs" }] },
        { text: " or " },
        { link: "https://y.dev", children: [{ text: "https://y.dev" }] },
        { text: ". " },
        { mention: id },
        { text: " " },
        { code: "**no**" },
        { text: " snake_case_name" },
      ],
    );
    assert.deepEqual(parseInline("[x](javascript:alert(1))"), [
      { text: "[x](javascript:alert(1))" },
    ]);
  });
});

describe("voice", () => {
  const nameOf = (id: string): string | undefined => names.agents.get(id);

  it("reads the words of a message, not its Markdown or code", () => {
    const text = [
      "## Done",
      `**Deploy** finished, <@${BOB}> checked it.`,
      "- see [the log](https://x.dev/log) and `plan.md`",
      "```sh\nrm -rf /tmp/x\n```",
      "Keep scribe_v3_live and _this_ word, see https://y.dev/a.",
    ].join("\n");
    assert.equal(
      speakable(text, nameOf),
      "Done Deploy finished, @bob checked it. see the log and plan.md (code) Keep scribe_v3_live and this word, see (link)",
    );
  });

  it("cuts long replies at a sentence", () => {
    const out = speakable("This is one sentence. ".repeat(100), nameOf);
    assert.ok(out.length < 1600);
    assert.ok(out.endsWith("sentence. The rest is in the chat."));
  });
});

describe("live transcription", () => {
  it("reads partial, committed and error messages", () => {
    assert.deepEqual(liveEvent('{"message_type":"partial_transcript","text":"hel"}'), {
      kind: "partial",
      text: "hel",
    });
    assert.deepEqual(liveEvent('{"message_type":"committed_transcript","text":"hello"}'), {
      kind: "committed",
      text: "hello",
    });
    assert.deepEqual(liveEvent('{"message_type":"auth_error","error":"bad token"}'), {
      kind: "error",
      message: "bad token",
    });
    assert.deepEqual(liveEvent('{"message_type":"session_started"}'), { kind: "other" });
    assert.deepEqual(liveEvent("not json"), { kind: "other" });
  });

  it("encodes samples as 16-bit little-endian PCM in base64", () => {
    const bytes = Buffer.from(pcm16Base64(new Float32Array([0, 1, -1, 2])), "base64");
    assert.deepEqual(
      [...new Int16Array(bytes.buffer, bytes.byteOffset, 4)],
      [0, 32767, -32767, 32767],
      "clipped to -1..1",
    );
    assert.equal(pcm16Base64(new Float32Array(0)), "");
  });
});

describe("agent order", () => {
  const a = (id: string, pinned = false): { id: string; name: string; pinned: boolean } => ({
    id,
    name: id,
    pinned,
  });
  it("keeps pinned first, then the saved order, new agents last by name", () => {
    const got = orderAgents([a("c"), a("b"), a("p", true), a("a"), a("z")], ["z", "c", "gone"]);
    assert.deepEqual(
      got.map((x) => x.id),
      ["p", "z", "c", "a", "b"],
    );
  });

  it("moves an id before or after another", () => {
    assert.deepEqual(moveId(["a", "b", "c"], "c", "a", false), ["c", "a", "b"]);
    assert.deepEqual(moveId(["a", "b", "c"], "a", "c", true), ["b", "c", "a"]);
    assert.deepEqual(moveId(["a", "b"], "a", "a", true), ["a", "b"]);
    assert.deepEqual(moveId(["a", "b"], "a", "x", true), ["a", "b"], "unknown target: unchanged");
  });
});

describe("modelOptions", () => {
  const models = [
    { id: "claude-x", name: "Claude X" },
    { id: "gpt-x", name: null },
  ];

  it("labels models, keeps a current id the list lacks, and has none without a list", () => {
    assert.deepEqual(modelOptions(models, ""), [
      ["claude-x", "Claude X (claude-x)"],
      ["gpt-x", "gpt-x"],
    ]);
    assert.equal(modelOptions(models, "gpt-x").length, 2);
    assert.deepEqual(modelOptions(models, "my-finetune")[0], ["my-finetune", "my-finetune"]);
    assert.deepEqual(modelOptions([], "my-finetune"), [], "no list: the field is typed");
  });

  it("keeps a saved voice id the list lacks", () => {
    const voices: [string, string][] = [["v1", "Rachel"]];
    assert.deepEqual(withSaved(voices, "v9"), [
      ["v9", "v9"],
      ["v1", "Rachel"],
    ]);
    assert.equal(withSaved(voices, "v1"), voices);
    assert.equal(withSaved(voices, ""), voices);
    assert.deepEqual(withSaved([], "v9"), [], "no list: the field is typed");
  });
});

describe("usage", () => {
  const group = (label: string, cost: number | null, unknown = 0): UsageGroup => ({
    key: label,
    label,
    calls: 1,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    cost_usd: cost,
    unknown_cost_calls: unknown,
    not_tracked_calls: cost === null && unknown === 0 ? 1 : 0,
  });

  it("turns ranges into local-midnight bounds", () => {
    const now = new Date(2026, 10, 20, 15, 30);
    assert.deepEqual(rangeOf("today", now, { from: "", to: "" }), {
      from: new Date(2026, 10, 20).toISOString(),
      to: null,
    });
    assert.equal(
      rangeOf("7d", now, { from: "", to: "" })?.from,
      new Date(2026, 10, 14).toISOString(),
    );
    assert.deepEqual(rangeOf("custom", now, { from: "2026-10-30", to: "2026-11-01" }), {
      from: new Date(2026, 9, 30).toISOString(),
      to: new Date(2026, 10, 2).toISOString(),
    });
    assert.equal(rangeOf("custom", now, { from: "2026-11-02", to: "2026-11-01" }), null);
    assert.equal(rangeOf("custom", now, { from: "", to: "2026-11-01" }), null);
  });

  it("lists every day of a range in the daemon's time zone", () => {
    const now = new Date();
    assert.deepEqual(zoneDays("2026-11-18T23:00:00.000Z", "2026-11-20T23:00:00.000Z", now, "UTC"), [
      "2026-11-18",
      "2026-11-19",
      "2026-11-20",
    ]);
    assert.deepEqual(
      zoneDays("2026-11-18T23:00:00.000Z", "2026-11-20T23:00:00.000Z", now, "Europe/Berlin"),
      ["2026-11-19", "2026-11-20"],
      "Berlin midnights",
    );
    assert.deepEqual(
      zoneDays("2026-11-20T00:00:00.000Z", null, new Date("2026-11-20T09:00:00Z"), "UTC"),
      ["2026-11-20"],
    );
    assert.deepEqual(
      zoneDays("2026-03-28T23:00:00.000Z", "2026-03-30T22:00:00.000Z", now, "Europe/Berlin"),
      ["2026-03-29", "2026-03-30"],
      "a 23-hour DST day",
    );
  });

  it("stacks the biggest groups per day and folds the rest", () => {
    const daily = (label: string, costs: Record<string, number | null>): UsageGroup => {
      const days = Object.entries(costs).map(([date, cost]) => ({
        ...group(label, cost, cost === null ? 1 : 0),
        date,
      }));
      const known = days.filter((d) => d.cost_usd !== null);
      return {
        ...group(label, known.length > 0 ? known.reduce((s, d) => s + (d.cost_usd ?? 0), 0) : null),
        days,
      };
    };
    const groups = [
      daily("zed", { "2026-11-19": 9 }),
      daily("amy", { "2026-11-18": 1, "2026-11-19": 2 }),
      daily("bob", { "2026-11-19": null }),
      daily("cy", { "2026-11-18": 4 }),
      daily("dee", { "2026-11-19": 0.5 }),
      daily("eve", { "2026-11-19": 0.25 }),
    ];
    const stack = stackDays(groups, ["2026-11-18", "2026-11-19", "2026-11-20"], "cost");
    assert.deepEqual(
      stack.series.map((s) => s.label),
      ["amy", "cy", "dee", "zed", "Other"],
      "top four by cost, named in order; eve and bob fold",
    );
    assert.deepEqual(
      stack.columns.map((c) => [c.date, c.values, c.total, c.unknown]),
      [
        ["2026-11-18", [1, 4, 0, 0, 0], 5, false],
        ["2026-11-19", [2, 0, 0.5, 9, 0.25], 11.75, true],
        ["2026-11-20", [0, 0, 0, 0, 0], 0, false],
      ],
    );
    assert.equal(stackDays(groups.slice(0, 5), [], "cost").series.length, 5, "five fit");
  });

  it("charts tokens when no cost is known", () => {
    assert.equal(chartMetric(group("a", 1.5)), "cost");
    assert.equal(chartMetric(group("a", null, 2)), "tokens");
    assert.equal(chartMetric(group("a", 0)), "tokens", "only free models");
  });

  it("picks round axis ticks and labels them", () => {
    assert.deepEqual(niceTicks(3.2), [0, 1, 2, 3, 4]);
    assert.deepEqual(niceTicks(1), [0, 0.5, 1]);
    assert.deepEqual(niceTicks(0.004), [0, 0.001, 0.002, 0.003, 0.004]);
    assert.deepEqual(niceTicks(0), [0, 1]);
    assert.equal(tickLabel(0.5, 0.5, "cost"), "$0.50");
    assert.equal(tickLabel(0.002, 0.001, "cost"), "$0.002");
    assert.equal(tickLabel(2000, 1000, "cost"), "$2,000");
    assert.equal(tickLabel(20_000, 10_000, "tokens"), "20K");
  });

  it("sorts with unknown costs last and labels them plainly", () => {
    const rows = [group("b", 2), group("a", null, 3), group("c", 0.004)];
    assert.deepEqual(
      sortGroups(rows, "cost_usd", true).map((g) => g.label),
      ["b", "c", "a"],
    );
    assert.deepEqual(
      sortGroups(rows, "cost_usd", false).map((g) => g.label),
      ["c", "b", "a"],
    );
    assert.deepEqual(
      sortGroups(rows, "label", false).map((g) => g.label),
      ["a", "b", "c"],
    );
    assert.deepEqual(rows.map(costLabel), ["$2.00", "unknown", "<$0.01"]);
    assert.equal(costLabel(group("d", null)), "not tracked");
    assert.equal(costLabel(group("e", 1, 2)), "$1.00 + unknown");
  });

  it("writes CSV that spreadsheets read as text", () => {
    assert.equal(
      toCsv([
        ["name", "cost"],
        ['a "b", c', 1.5],
        ["=SUM(A1)", null],
      ]),
      'name,cost\r\n"a ""b"", c",1.5\r\n\'=SUM(A1),\r\n',
    );
  });
});
