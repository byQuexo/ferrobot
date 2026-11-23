import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  DaemonClient,
  type MailMessage,
  type Provider,
  type ProviderKind,
  type StoredEvent,
} from "../src/api.ts";
import { ACCENTS, DEFAULT_APPEARANCE, readAppearance } from "../src/lib/appearance.ts";
import { hash } from "../src/lib/avatar.ts";
import { characterOf, deriveCharacter, moodOf, rerollCharacter } from "../src/lib/character.ts";
import {
  computerSummary,
  linkLabel,
  relativeTime,
  rowLine,
  statusOf,
  stepLabel,
  stepsLabel,
  toolLabel,
  toolsSummary,
} from "../src/lib/format.ts";
import { GUIDES, splitWikiLinks } from "../src/lib/guides.ts";
import { providerReady, welcomeStep } from "../src/lib/welcome.ts";
import { buildTimeline, foldSteps, sharedFiles, withoutRoomTurns } from "../src/lib/timeline.ts";
import {
  CallScript,
  firstClause,
  callChanged,
  callClock,
  callTurn,
  isPass,
  operatorSaid,
  roomTitle,
  splitSentences,
  type CallState,
  type Line,
} from "../src/lib/call.ts";
import { CATALOG, isAdded } from "../src/lib/catalog.ts";
import { checkDot, checkLabel, ownAppEntry } from "../src/lib/connectors.ts";
import { claudeCodeState } from "../src/lib/claude-code.ts";
import { diagnosticsReport } from "../src/lib/diagnostics.ts";
import { parseBlocks, parseInline } from "../src/lib/markdown.ts";
import { modelOptions, withSaved } from "../src/lib/models.ts";
import { moveId, orderAgents, pickSelection } from "../src/lib/order.ts";
import { keyLabel, matchShortcut, SHORTCUTS, type KeyPress } from "../src/lib/shortcuts.ts";
import { pollOutcome } from "../src/lib/update.ts";
import { Speaker, hasVoiceKey, liveEvent, pcm16Base64, speakable } from "../src/lib/voice.ts";
import {
  chartMetric,
  costLabel,
  freshTokens,
  niceTicks,
  rangeOf,
  resetLabel,
  shareLabel,
  slotOf,
  sortGroups,
  stackDays,
  tickLabel,
  toCsv,
  zoneDays,
} from "../src/lib/usage.ts";
import type {
  Connector,
  Diagnostics,
  UpdateInfo,
  UsageGroup,
  VoiceProvider,
  VoiceView,
} from "../src/api.ts";

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
    // Trouble, then an open card, outrank what the agent does.
    assert.equal(moodOf("error"), "trouble");
    assert.equal(moodOf("working", { trouble: true, waiting: true }), "trouble");
    assert.equal(moodOf("awake", { waiting: true }), "waiting");
    assert.equal(moodOf(undefined), "idle");
  });
});

describe("format", () => {
  it("puts a credential problem over presence", () => {
    const problem = (kind: "missing" | "rejected") => ({
      kind,
      reason: "provider oai has no API key: add it in Settings, Vault",
      since: "2026-01-01T12:00:00Z",
    });
    assert.deepEqual(statusOf({ presence: "awake", credential_error: null }), {
      presence: "awake",
      label: "Awake",
    });
    assert.deepEqual(statusOf({ presence: "sleeping", credential_error: problem("missing") }), {
      presence: "error",
      label: "Key missing",
    });
    assert.equal(
      statusOf({ presence: "awake", credential_error: problem("rejected") }).label,
      "Key rejected",
    );
    const noTools = {
      model: "m",
      reason: "or can't run m with tools",
      since: "2026-01-01T12:00:00Z",
    };
    assert.deepEqual(
      statusOf({ presence: "awake", credential_error: null, tools_unsupported: noTools }),
      { presence: "error", label: "Model can't use tools" },
    );
    assert.equal(
      statusOf({
        presence: "awake",
        credential_error: problem("missing"),
        tools_unsupported: noTools,
      }).label,
      "Key missing",
    );
  });
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

  it("shortens links for chips", () => {
    const cases: [string, string][] = [
      [
        "https://www.amazon.de/dp/B0XXXX/ref=...?ref_=abc&social_share=xyz&foo=1&bar=2",
        "amazon.de/dp/…?ref_=…&social_share=…&…",
      ],
      ["https://github.com/byQuexo", "github.com/byQuexo"],
      ["https://www.example.com/", "example.com"],
      ["https://github.com/byQuexo/ferrobot/pull/38", "github.com/byQuexo/ferrobot/pull/38"],
      ["https://de.wikipedia.org/wiki/Straße", "de.wikipedia.org/wiki/Straße"],
      [
        "http://127.0.0.1:7878/artifacts/abc/report.html?sig=abcdef&exp=1",
        "127.0.0.1:7878/artifacts/…?sig=…&exp=…",
      ],
      [
        "https://docs.example.com/guide/install/linux#before-you-start",
        "docs.example.com/guide/…#…",
      ],
      [
        "https://example.com/a-very-long-single-path-segment?flag",
        "example.com/a-very-long-single-path-segment?flag",
      ],
    ];
    for (const [url, label] of cases) assert.equal(linkLabel(url), label, url);
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

  it("leaves call room messages and turns out of the chat, but keeps cards and files", () => {
    const fromRoom = msg(2, { kind: "user" }, ME, "in the room", "2026-01-01T10:01:00Z");
    const chat = withoutRoomTurns(
      [
        msg(1, { kind: "user" }, ME, "just me", "2026-01-01T10:00:00Z"),
        { ...fromRoom, room_id: 3 },
      ],
      [
        ev(10, "message_in", { message_id: 1 }, "2026-01-01T10:00:01Z", "t1"),
        ev(11, "assistant_text", { text: "Hi." }, "2026-01-01T10:00:02Z", "t1"),
        ev(12, "message_in", { message_id: 2, room_id: 3 }, "2026-01-01T10:01:01Z", "t2"),
        ev(13, "tool_use", { id: "c", name: "create_agent" }, "2026-01-01T10:01:02Z", "t2"),
        ev(14, "assistant_text", { text: "Said in the room." }, "2026-01-01T10:01:03Z", "t2"),
        ev(15, "turn_done", { outcome: "failed" }, "2026-01-01T10:01:04Z", "t2"),
        ev(16, "question", { card: { questions: [] } }, "2026-01-01T10:01:05Z", ""),
      ],
      new Map([
        ["t2", "streaming in the room"],
        ["t3", "streaming here"],
      ]),
    );
    const items = buildTimeline(ME, chat.messages, chat.events, chat.live, names);
    assert.deepEqual(
      items.map((i) => i.type),
      ["user", "assistant", "question", "assistant"],
    );
    assert.ok(items[3]?.type === "assistant" && items[3].text === "streaming here");
    assert.equal(chat.roomTurn, true);
    const room = ev(12, "message_in", { message_id: 2, room_id: 3 }, "2026-01-01T10:01:01Z", "t2");
    const after = ev(17, "message_in", { message_id: 4 }, "2026-01-01T10:02:00Z", "t3");
    assert.equal(withoutRoomTurns([], [room, after], new Map()).roomTurn, false);
  });

  it("shows a credential or tools problem once, instead of its turn's error", () => {
    const at = "2026-01-01T12:00:00Z";
    const notice = (id: number, turn: string): StoredEvent => ({
      ...ev(id, "credential_error", { kind: "missing", reason: "no key", turn_id: turn }, at),
      turn_id: null,
    });
    const noTools: StoredEvent = {
      ...ev(8, "tools_unsupported", { reason: "or can't run m with tools", turn_id: "t4" }, at),
      turn_id: null,
    };
    const items = buildTimeline(
      ME,
      [],
      [
        ev(1, "assistant_text", { text: "Looking." }, at, "t1"),
        notice(2, "t1"),
        ev(3, "assistant_text", { text: "API Error: 400 no key" }, at, "t1"),
        ev(4, "turn_done", { outcome: "completed", result: "API Error: 400 no key" }, at, "t1"),
        notice(5, "t2"),
        ev(6, "turn_done", { outcome: "failed", error: "API Error: 400" }, at, "t2"),
        ev(7, "turn_done", { outcome: "failed", error: "boom" }, at, "t3"),
        noTools,
        ev(9, "turn_done", { outcome: "failed", error: "model request failed with 404" }, at, "t4"),
      ],
      new Map(),
      names,
    );
    assert.deepEqual(
      items.map((i) => (i.type === "error" || i.type === "assistant" ? i.text : i.type)),
      [
        "Looking.",
        "Key missing: no key. Messages wait until it is fixed.",
        "Key missing: no key. Messages wait until it is fixed.",
        "boom",
        "Model can't use tools: or can't run m with tools. Messages wait until you choose.",
      ],
    );
  });

  it("shows why a message was not delivered: its last attempt's error", () => {
    const at = "2026-01-01T12:00:00Z";
    const failed = (id: number): MailMessage => ({
      ...msg(id, { kind: "user" }, ME, "hi", at),
      status: "failed",
    });
    const attempt = (id: number, message: number, turn: string, error: string): StoredEvent[] => [
      ev(id, "message_in", { message_id: message }, at, turn),
      ev(id + 1, "turn_done", { outcome: "failed", error }, at, turn),
    ];
    const events = [...attempt(1, 1, "t1", "first try"), ...attempt(3, 1, "t2", "quota reached")];
    const delivered = msg(2, { kind: "user" }, ME, "ok", at);
    const items = buildTimeline(ME, [failed(1), delivered], events, new Map(), names);
    const errors = items.map((i) => (i.type === "user" ? (i.error ?? null) : undefined));
    assert.deepEqual(
      errors.filter((e) => e !== undefined),
      ["quota reached", null],
    );
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

  it("notes agents created or changed in auto mode", () => {
    const created = { name: "scout", id: BOB, by: "ada", auto: true };
    const items = buildTimeline(
      ME,
      [],
      [
        ev(1, "agent_created", created, "2026-01-01T12:00:00Z", ""),
        ev(2, "agent_tools_changed", created, "2026-01-01T12:01:00Z", ""),
      ],
      new Map(),
      names,
    );
    assert.deepEqual(
      items.map((i) => (i.type === "system" ? i.text : null)),
      ["ada created scout (auto mode)", "ada changed the tools of scout (auto mode)"],
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

  it("joins wrapped list items and keeps the number after a code block", () => {
    const md = "1. Install it\n   and start it.\n\n   ```\n   run\n   ```\n\n2. Done\n\n   later";
    assert.deepEqual(parseBlocks(md), [
      { kind: "list", ordered: true, items: [{ text: "Install it and start it.", depth: 0 }] },
      { kind: "code", text: "run" },
      { kind: "list", ordered: true, start: 2, items: [{ text: "Done", depth: 0 }] },
      { kind: "para", text: "   later" },
    ]);
  });
});

describe("welcome", () => {
  const provider = (kind: ProviderKind, has_key: boolean): Provider => ({
    id: kind,
    name: kind,
    kind,
    has_key,
    created_at: "2026-01-01T00:00:00Z",
  });

  it("picks the first-run step", () => {
    const base: Parameters<typeof welcomeStep>[0] = {
      agents: 0,
      providers: [],
      daemonKey: false,
      skipped: false,
    };
    const cases: [Partial<typeof base>, string | null][] = [
      [{}, "provider"],
      [{ providers: [provider("openai", false)] }, "key"],
      [{ providers: [provider("openai", false), provider("openai", true)] }, "meet"],
      [{ providers: [provider("ollama", false)] }, "meet"],
      [{ daemonKey: true }, "meet"],
      [{ agents: 1 }, null],
      [{ agents: 1, providers: [provider("openai", true)] }, null],
      [{ skipped: true }, null],
      [{ providers: null }, null],
    ];
    for (const [input, step] of cases)
      assert.equal(welcomeStep({ ...base, ...input }), step, JSON.stringify(input));
    assert.equal(providerReady(provider("anthropic", false)), false);
    assert.equal(providerReady(provider("vertex", false)), true);
    // Without a key Codex needs a ChatGPT sign-in, which only the welcome itself checks.
    assert.equal(providerReady(provider("codex", false)), false);
    assert.equal(welcomeStep({ ...base, providers: [provider("codex", false)] }), "key");
  });
});

describe("ui polish", () => {
  const agent = {
    presence: "awake" as const,
    credential_error: null,
    tools_unsupported: null,
    waiting: 0,
    last_reply: null,
    description: "Reviews code",
  };
  const now = Date.parse("2026-11-23T12:00:00Z");

  it("puts a problem, an open card, work, then the last reply on the sidebar row", () => {
    assert.deepEqual(rowLine(agent, now), { text: "Reviews code", tone: "", time: "" });
    const replied = { ...agent, last_reply: { text: "Done.", at: "2026-11-23T11:55:00Z" } };
    assert.deepEqual(rowLine(replied, now), { text: "Done.", tone: "", time: "5 min" });
    assert.equal(rowLine({ ...replied, presence: "working" }, now).text, "Working…");
    const md = {
      ...agent,
      last_reply: { text: "# AI news: **Nvidia** `x`", at: "2026-11-23T11:55:00Z" },
    };
    assert.equal(rowLine(md, now).text, "AI news: Nvidia x");
    assert.equal(rowLine({ ...replied, waiting: 2 }, now).text, "2 questions for you");
    assert.equal(rowLine({ ...replied, waiting: 2, presence: "error" }, now).tone, "bad");
  });

  it("labels steps, the computer and the Claude Code computer tools in plain words", () => {
    assert.equal(stepsLabel(1, 0.2), "1 step");
    assert.equal(stepsLabel(3, 40.4), "3 steps · 40 s");
    assert.equal(
      computerSummary("running", { idle_s: 60, warm_after_s: 300 }),
      "Idle 1 min · pauses in 4 min",
    );
    assert.equal(
      computerSummary("running", { idle_s: 1, idle_timeout_s: 900 }),
      "Busy just now · sleeps in 14 min",
    );
    assert.equal(computerSummary("paused", { idle_s: 999 }), "Paused · resumes quickly");
    const none = (): undefined => undefined;
    assert.equal(
      stepLabel("mcp__ferrobot__bash", { command: "ls" }, none),
      "Working on its computer",
    );
    assert.equal(stepLabel("mcp__ferrobot__remember", {}, none), "Taking notes");
  });

  it("gives a failed turn one quiet line unless a bubble already says why", () => {
    const at = "2026-11-23T12:00:00Z";
    const mail = (id: number, sender: MailMessage["sender"]): MailMessage => ({
      ...msg(id, sender, ME, "hi", at),
      status: "failed",
    });
    const fail = (id: number, turn: string, message: number | null): StoredEvent[] => [
      ev(id, "message_in", message === null ? {} : { message_id: message }, at, turn),
      ev(id + 1, "turn_done", { outcome: "failed", error: "boom" }, at, turn),
    ];
    const events = [
      ...fail(1, "greet", null),
      ...fail(3, "u1", 1),
      ...fail(5, "b1", 2),
      ...fail(7, "b2", 2),
    ];
    const messages = [mail(1, { kind: "user" }), mail(2, { kind: "agent", id: BOB })];
    const quiet = buildTimeline(ME, messages, events, new Map(), names).flatMap((i) =>
      i.type === "error" ? [i.quiet === true] : [],
    );
    // greeting: yes; the user's message: its bubble; bob's mail: only the last retry.
    assert.deepEqual(quiet, [true, false, false, true]);
  });

  it("folds finished tool runs into one line and keeps the running one apart", () => {
    const tool = (n: number, at: string) =>
      ({
        type: "tool",
        key: `t${String(n)}`,
        at,
        name: "bash",
        input: {},
        output: "",
        isError: false,
        denied: null,
        turn: "t",
      }) as const;
    const text = {
      type: "assistant",
      key: "a",
      at: "2026-11-23T12:01:00Z",
      text: "ok",
      streaming: false,
    } as const;
    const items = [
      tool(1, "2026-11-23T12:00:00Z"),
      tool(2, "2026-11-23T12:00:40Z"),
      text,
      tool(3, "2026-11-23T12:02:00Z"),
    ];
    const done = foldSteps(items, false);
    assert.deepEqual(
      done.shown.map((i) => i.type),
      ["steps", "assistant", "steps"],
    );
    const first = done.shown[0];
    assert.equal(first?.type === "steps" ? first.seconds : null, 40);
    const running = foldSteps(items, true);
    assert.deepEqual(
      running.shown.map((i) => i.type),
      ["steps", "assistant"],
    );
    assert.deepEqual(
      running.live.map((t) => t.key),
      ["t3"],
    );
  });
});

describe("claude code", () => {
  it("says what the host's claude needs before agents can use it", () => {
    const base = {
      installed: true,
      logged_in: true,
      version: "2.1.330 (Claude Code)",
      auth_method: "claude.ai",
    };
    assert.deepEqual(claudeCodeState(base), {
      ready: true,
      text: "2.1.330 (Claude Code) · logged in with claude.ai",
    });
    assert.match(claudeCodeState({ ...base, logged_in: false }).text, /claude auth login/);
    assert.equal(claudeCodeState({ ...base, logged_in: false }).ready, false);
    assert.match(claudeCodeState({ ...base, installed: false }).text, /not installed/);
    assert.equal(claudeCodeState(null).ready, false);
  });
});

describe("guides", () => {
  it("splits wiki links in GitHub's Text|Page order", () => {
    assert.deepEqual(
      splitWikiLinks(
        "See [[FAQ]], [[Getting Started|Getting-Started]] and [[proxy|Usage and Costs#api]].",
      ),
      [
        { text: "See " },
        { page: "FAQ", label: "FAQ" },
        { text: ", " },
        { page: "Getting-Started", label: "Getting Started" },
        { text: " and " },
        { page: "Usage-and-Costs", label: "proxy" },
        { text: "." },
      ],
    );
    assert.deepEqual(splitWikiLinks("no [link] here"), [{ text: "no [link] here" }]);
  });

  it("lists each guide once", () => {
    const pages = GUIDES.map((g) => g.page);
    assert.equal(new Set(pages).size, pages.length);
    assert.ok(!pages.includes("Architecture"));
  });
});

describe("tools summary", () => {
  it("summarises tool groups and connectors", () => {
    assert.equal(toolsSummary(9, 9, 0), "All tools");
    assert.equal(toolsSummary(7, 9, 0), "7 of 9 tools");
    assert.equal(toolsSummary(0, 9, 1), "No tools + 1 connector");
    assert.equal(toolsSummary(8, 8, 2), "All tools + 2 connectors");
  });
});

describe("voice", () => {
  const nameOf = (id: string): string | undefined => names.agents.get(id);

  it("needs the chosen provider's key to dictate", () => {
    const view = (provider: VoiceView["provider"], keyed: VoiceProvider[]): VoiceView =>
      ({
        provider,
        providers: (["elevenlabs", "openrouter"] as const).map((kind) => ({
          kind,
          has_key: keyed.includes(kind),
        })),
      }) as VoiceView;
    assert.equal(hasVoiceKey(view("elevenlabs", ["elevenlabs"])), true);
    assert.equal(hasVoiceKey(view("openrouter", ["elevenlabs"])), false);
    assert.equal(hasVoiceKey(view(null, ["elevenlabs"])), false);
  });

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

describe("voice call", () => {
  it("splits sentences, not abbreviations, numbers, links or code", () => {
    const text = [
      "Hi Tim! Dr. Smith checked it, e.g. the 3.5 build on x.dev/log.",
      "Am 21. November ging es los.",
      "It shipped in 2024. Done?",
      "```sh\nrm -rf /tmp/x\n```",
      "Last one",
    ].join("\n");
    assert.deepEqual(splitSentences(text), {
      done: [
        "Hi Tim!",
        "Dr. Smith checked it, e.g. the 3.5 build on x.dev/log.",
        "Am 21. November ging es los.",
        "It shipped in 2024.",
        "Done?",
        "```sh\nrm -rf /tmp/x\n```",
      ],
      rest: "Last one",
    });
    assert.deepEqual(splitSentences("Wait for the space."), {
      done: [],
      rest: "Wait for the space.",
    });
  });

  it("speaks streamed text in the same sentences however it is chunked, each once", () => {
    const text =
      'Sure, I\'ll check. The build (v1.2) passed! Dr. Who said "ok." Next:\n- one\n- two\nBye.';
    const whole = new CallScript();
    const once = [...whole.text("a", "t0", text), ...whole.done("t0")].map((l) => l.text);
    for (const size of [1, 2, 3, 7]) {
      const script = new CallScript();
      const said: Line[] = [];
      for (let i = 0; i < text.length; i += size)
        said.push(...script.delta("a", "t1", text.slice(i, i + size)));
      said.push(...script.text("a", "t1", text.replace("Dr.", "<@dr>")), ...script.done("t1"));
      assert.deepEqual(
        said.map((l) => l.text),
        once,
        `chunks of ${String(size)}`,
      );
    }
    assert.equal(once[0], "Sure, I'll check.");
    assert.equal(once.at(-1), "Bye.");
  });

  it("lets one agent finish its text block before the next speaks", () => {
    const script = new CallScript();
    const said: Line[] = [];
    said.push(...script.delta("a", "ta", "A one. A t"));
    said.push(...script.delta("b", "tb", "B one. B two. "));
    said.push(...script.delta("a", "ta", "wo. "));
    said.push(...script.text("a", "ta", "A one. A two."));
    said.push(...script.text("b", "tb", "B one. B two."));
    assert.deepEqual(
      said.map((l) => `${l.agent}:${l.text}`),
      ["a:A one.", "a:A two.", "b:B one.", "b:B two."],
    );
  });

  it("never speaks a (pass) and keeps hushed turns quiet", () => {
    const script = new CallScript();
    assert.deepEqual(script.delta("a", "t1", "(pa"), []);
    assert.deepEqual(script.delta("a", "t1", "ss)"), []);
    assert.deepEqual(script.text("a", "t1", "(pass)"), []);
    assert.deepEqual(script.text("b", "t2", "(Pass)."), []);
    assert.ok(isPass(" (pass)\n") && !isPass("(pass) on that, but yes"));
    assert.ok(isPass("pass") && isPass("Pass.") && !isPass("passing on"));

    assert.equal(script.delta("c", "t3", "Long answer. And more").length, 1);
    assert.ok(script.speaking("c"));
    script.hush();
    assert.deepEqual(script.delta("c", "t3", " words. Still going. "), []);
    assert.deepEqual(script.done("t3"), []);
    assert.ok(!script.speaking("c"));
  });

  it("lets only the goodbye of the agent that hung up finish when the call ends", () => {
    const script = new CallScript();
    assert.equal(script.delta("b", "tb", "Still talking. And").length, 1, "b has the floor");
    assert.deepEqual(script.delta("a", "ta", "Bye now. "), [], "a waits for b");
    assert.ok(script.active);
    assert.deepEqual(
      script.end("a").map((l) => `${l.agent}:${l.text}`),
      ["a:Bye now."],
      "a's goodbye goes on at once",
    );
    assert.deepEqual(script.delta("b", "tb", " more. "), [], "b is cut off");
    assert.deepEqual(script.text("a", "ta", "Bye now. See you."), []);
    assert.ok(script.active, "a may still speak");
    assert.deepEqual(script.done("ta"), []);
    assert.ok(!script.active, "nothing left to wait for");
    const ended = new CallScript();
    ended.delta("b", "tb", "Talking. ");
    assert.deepEqual(ended.end(null), [], "the operator ended it: everyone stops");
    assert.deepEqual(ended.delta("b", "tb", "More. "), []);
  });

  it("speaks only the call's turns: the room's, or the host's without one", () => {
    const call: CallState = {
      agents: ["a", "b"],
      host: "a",
      since: 0,
      muted: false,
      hearing: true,
      ending: false,
      room: null,
    };
    assert.ok(callTurn(call, "a", null), "one on one: the host's turns");
    assert.ok(!callTurn(call, "b", null), "nobody else's");
    const room = { ...call, room: 7 };
    assert.ok(callTurn(room, "b", 7) && callTurn(room, "a", 7));
    assert.ok(!callTurn(room, "a", null), "a member's own chat is not heard in a room call");
    assert.ok(!callTurn(room, "c", null), "a new agent's greeting is not heard");
    assert.ok(!callTurn(room, "b", 6), "nor an old room");
  });

  it("drops every other voice, the one playing included, once the call ends", async () => {
    const global = globalThis as unknown as { Audio?: unknown };
    const playing: FakeAudio[] = [];
    class FakeAudio {
      onended: (() => void) | null = null;
      onerror: (() => void) | null = null;
      onpause: (() => void) | null = null;
      preload = "";
      src = "";
      play(): Promise<void> {
        playing.push(this);
        return Promise.resolve();
      }
      pause(): void {
        this.onpause?.();
      }
    }
    const before = global.Audio;
    global.Audio = FakeAudio;
    try {
      const voices: (string | null)[] = [];
      const speaker = new Speaker(
        (text) => `/v1/voice/speech?text=${encodeURIComponent(text)}`,
        (err) => {
          throw err;
        },
      );
      speaker.onSpeaking = (agent) => voices.push(agent);
      const tick = (): Promise<void> => new Promise((r) => setTimeout(r, 5));
      speaker.say("a one", "a");
      speaker.say("b bye", "b");
      speaker.say("a two", "a");
      await tick();
      speaker.keepOnly("b");
      speaker.say("a three", "a");
      await tick();
      playing.at(-1)?.onended?.();
      await tick();
      assert.deepEqual(voices, ["a", "b", null], "a is cut off, b's goodbye plays");
      assert.equal(playing.length, 2);
      assert.ok(playing[1]?.src.endsWith("text=b%20bye"), "streams b's goodbye from its link");
    } finally {
      global.Audio = before;
    }
  });

  it("starts speaking at the first clause of a long first sentence", () => {
    const script = new CallScript();
    const first = script.delta("a", "t1", "England is a strong alternative, especially in late");
    assert.deepEqual(
      first.map((l) => l.text),
      ["England is a strong alternative,"],
    );
    const more = script.delta("a", "t1", " September, when the coast is at its best. Then more,");
    assert.deepEqual(
      more.map((l) => l.text),
      ["especially in late September, when the coast is at its best."],
      "only the first clause goes early",
    );
    assert.equal(firstClause("Short, then more"), null, "at least 20 characters first");
    assert.equal(
      firstClause("A price of 1,5 million euros or more"),
      null,
      "a comma needs a space",
    );
    assert.equal(firstClause("```js\nconst a = 1, b = 2;\n"), null);
  });

  it("cuts a long turn with a notice", () => {
    const script = new CallScript();
    const said = script.text("a", "t1", "This is one sentence. ".repeat(100));
    assert.ok(said.map((l) => l.text).join(" ").length < 1600);
    assert.equal(said.at(-1)?.text, "The rest is in the chat.");
  });

  it("interrupts only with real words that are not the agent's own echo", () => {
    const said = "I deployed the build to staging.";
    assert.ok(!operatorSaid("", false, ""));
    assert.ok(operatorSaid("yes", false, ""), "one word answers when nobody speaks");
    assert.ok(operatorSaid("staging", false, said), "a one-word answer is never echo");
    assert.ok(!operatorSaid("yes", true, said), "one word does not interrupt");
    assert.ok(!operatorSaid("hmm …", true, said));
    assert.ok(operatorSaid("wait stop", true, said));
    assert.ok(!operatorSaid("deployed the build", true, said), "echo");
    assert.ok(!operatorSaid("deployed a bill to staging", false, said), "echo after it ended");
    assert.ok(operatorSaid("no, use production instead", true, said));
  });

  it("follows the daemon's call events", () => {
    const call: CallState = {
      agents: ["a", "b"],
      host: "a",
      since: 0,
      muted: true,
      hearing: true,
      ending: false,
      room: null,
    };
    assert.equal(callChanged(null, ["a"]), null, "no call here, nothing to follow");
    assert.deepEqual(callChanged(call, ["a", "b", "c"], 7), {
      ...call,
      agents: ["a", "b", "c"],
      room: 7,
    });
    assert.deepEqual(callChanged(call, ["a"]), { ...call, agents: ["a"] });
    assert.deepEqual(callChanged(call, ["b"]), { ...call, agents: ["b"], host: "b" });
    const ended = callChanged({ ...call, room: 7 }, []);
    assert.deepEqual(ended, { ...call, agents: [], hearing: false, ending: true, room: 7 });
    assert.equal(callChanged(ended, ["a"]), ended, "an ended call stays ended");
  });

  it("names a room after who took part", () => {
    const names = new Map([
      ["a", "team-lead"],
      ["b", "product-compare"],
    ]);
    const nameOf = (id: string): string | undefined => names.get(id);
    assert.equal(roomTitle(["a", "gone", "b"], nameOf), "Call · team-lead, product-compare");
    assert.equal(roomTitle(["gone"], nameOf), "Call");
  });

  it("shows how long the call runs", () => {
    assert.equal(callClock(0), "0:00");
    assert.equal(callClock(42_900), "0:42");
    assert.equal(callClock(3_903_000), "1:05:03");
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

  it("gives providers their chart colour, share and fresh tokens", () => {
    const series = [
      { key: "a", label: "a" },
      { key: "b", label: "b" },
      { key: "", label: "Other" },
    ];
    assert.deepEqual(
      ["a", "b", "folded"].map((k) => slotOf(series, k)),
      ["s0", "s1", "other"],
    );
    assert.deepEqual(
      [shareLabel(1, 3), shareLabel(0.001, 3), shareLabel(0, 3), shareLabel(1, 0)],
      ["33%", "<1%", "0%", "0%"],
    );
    const t = { ...group("a", 1), input_tokens: 10, output_tokens: 5 };
    assert.equal(freshTokens({ ...t, cache_read_tokens: 1000, cache_write_tokens: 2 }), 17);
  });

  it("shows Claude Code's reset words as they are and Codex times locally", () => {
    assert.equal(
      resetLabel("Nov 26 at 6:59am (Europe/Berlin)"),
      "Nov 26 at 6:59am (Europe/Berlin)",
    );
    const local = resetLabel("2026-11-26T05:59:00Z");
    assert.match(local, /^Nov 26, \d{1,2}:\d{2}\s?[AP]M$/);
  });
});

describe("catalog", () => {
  it("only holds entries a connector accepts", () => {
    const names = new Set<string>();
    for (const e of CATALOG) {
      assert.match(e.name, /^[a-z][a-z0-9-]{0,15}$/, e.name);
      assert.ok(e.url.startsWith("https://"), e.url);
      assert.ok(!names.has(e.name), `duplicate ${e.name}`);
      names.add(e.name);
    }
  });

  it("prefills an own OAuth app only for OAuth servers", () => {
    for (const e of CATALOG.filter((c) => c.app)) {
      assert.equal(e.auth, "oauth", e.name);
      assert.ok(e.app?.issuer.startsWith("https://"), e.name);
      assert.ok(e.app?.scopes.length, e.name);
    }
    assert.equal(
      CATALOG.find((e) => e.name === "gmail")?.app?.issuer,
      "https://accounts.google.com",
    );
  });

  it("knows what is added by name or url", () => {
    const linear = CATALOG.find((e) => e.name === "linear");
    assert.ok(linear);
    assert.equal(isAdded(linear, [{ name: "work", url: linear.url }]), true);
    assert.equal(isAdded(linear, [{ name: "linear", url: "https://x" }]), true);
    assert.equal(isAdded(linear, [{ name: "other", url: "https://x" }]), false);
  });
});

describe("source update", () => {
  it("relaunches only once a new daemon answers", () => {
    const info = (state: UpdateInfo["state"]): UpdateInfo => ({
      version: "0.1.0",
      source_build: true,
      state,
      log: [],
    });
    assert.equal(pollOutcome(null), "wait");
    assert.equal(pollOutcome(info("running")), "wait");
    assert.equal(pollOutcome(info("restarting")), "wait");
    assert.equal(pollOutcome(info("idle")), "restarted");
    assert.equal(pollOutcome(info("failed")), "done");
    assert.equal(pollOutcome(info("up_to_date")), "done");
  });
});

describe("diagnostics", () => {
  const info: Diagnostics = {
    version: "0.2.0",
    build: "release",
    os: "macos",
    arch: "aarch64",
    backend: "docker",
    driver: "claude",
    data_dir: "/Users/me/Library/Application Support/dev.ferrobot.desktop",
    storage: "local",
    sandbox_ready: false,
    sandbox_error: "Docker is not running",
    runner_image: "ghcr.io/byquexo/ferrobot-runner:0.2.0",
    runner_image_id: "sha256:8785127776e1aaaabbbbcccc",
    anthropic_key: false,
    agents: 1,
    providers: 2,
    connectors: 0,
    uptime_s: 3720,
  };

  it("reports versions, status, counts, the data folder and the log", () => {
    assert.equal(
      diagnosticsReport("0.1.0", info, ["line one", "line two"]),
      [
        "ferrobot diagnostics",
        "App: 0.1.0",
        "Daemon: 0.2.0 (release)",
        "System: macos aarch64",
        "Sandbox: docker, Docker is not running",
        "Runner image: ghcr.io/byquexo/ferrobot-runner:0.2.0 (8785127776e1)",
        "Driver: claude",
        "Anthropic key: not set",
        "Setup: 1 agent, 2 providers, 0 connectors",
        "Storage: local",
        "Uptime: 1 h 2 min",
        "Data folder: /Users/me/Library/Application Support/dev.ferrobot.desktop",
        "",
        "Daemon log (last 2 lines):",
        "line one",
        "line two",
      ].join("\n"),
    );
  });

  it("names a backend without images and a ready sandbox", () => {
    const report = diagnosticsReport(
      "0.1.0",
      {
        ...info,
        sandbox_ready: true,
        sandbox_error: null,
        runner_image: null,
        runner_image_id: null,
      },
      [],
    );
    assert.match(report, /^Sandbox: docker, ready$/m);
    assert.match(report, /^Runner image: none$/m);
    assert.match(report, /^Daemon log \(last 0 lines\):$/m);
  });
});

describe("connector status", () => {
  const base: Connector = {
    id: ME,
    name: "context7",
    url: "https://mcp.context7.com/mcp",
    created_at: "2026-11-21T10:00:00Z",
    has_token: false,
    signed_in: false,
    sign_in: "signed_out",
    has_client_secret: false,
    scopes: [],
    redirect_uri: "http://127.0.0.1:7878/v1/oauth/callback",
  };
  const at = "2026-11-21T10:00:01Z";

  it("comes from the last check, never from the sign-in alone", () => {
    const cases: [Connector, string, string][] = [
      [base, "not checked yet", "p-sleeping"],
      [{ ...base, check: { status: "ok", tools: 2, at } }, "connected · 2 tools", "p-awake"],
      [{ ...base, check: { status: "ok", tools: 1, at } }, "connected · 1 tool", "p-awake"],
      [{ ...base, check: { status: "needs_sign_in", at } }, "needs sign-in", "p-starting"],
      [
        { ...base, sign_in: "expired", check: { status: "needs_sign_in", at } },
        "sign-in expired, sign in again",
        "p-starting",
      ],
      [
        { ...base, check: { status: "failed", error: "cannot reach the server", at } },
        "failed: cannot reach the server",
        "p-error",
      ],
    ];
    for (const [c, label, dot] of cases) {
      assert.equal(checkLabel(c, false), label);
      assert.equal(checkDot(c, false), dot);
      assert.ok(!checkLabel(c, false).includes("not signed in"));
    }
    assert.equal(checkLabel(base, true), "checking…");
  });

  it("does not call a server that needs an own OAuth app connected without one", () => {
    const gmail = {
      ...base,
      url: "https://gmailmcp.googleapis.com/mcp/v1",
      check: { status: "ok" as const, tools: 23, at },
    };
    assert.equal(checkLabel(gmail, false), "needs your OAuth app");
    assert.equal(checkDot(gmail, false), "p-starting");
    const withApp = { ...gmail, client_id: "app.example" };
    assert.equal(checkLabel(withApp, false), "connected · 23 tools");
    assert.equal(ownAppEntry(" https://gmailmcp.googleapis.com/mcp/v1 ")?.name, "gmail");
    assert.equal(ownAppEntry("https://mcp.deepwiki.com/mcp"), undefined);
  });
});

describe("pickSelection", () => {
  const map = (...xs: { id: string; name: string; pinned?: boolean }[]) =>
    new Map(xs.map((x) => [x.id, x]));
  it("keeps the remembered agent until the list has loaded", () => {
    assert.equal(pickSelection(map(), "bob", false), undefined);
    assert.equal(pickSelection(map({ id: "bob", name: "bob" }), "bob", true), undefined);
  });
  it("falls back to the first pinned agent, then by name, once loaded", () => {
    const agents = map({ id: "c", name: "carol" }, { id: "f", name: "ferro", pinned: true });
    assert.equal(pickSelection(agents, "gone", true), "f");
    assert.equal(pickSelection(map(), "gone", true), null);
  });
});

describe("appearance", () => {
  it("reads stored choices and falls back to defaults for missing or unknown ones", () => {
    assert.deepEqual(
      readAppearance(() => null),
      DEFAULT_APPEARANCE,
    );
    const stored = new Map([
      ["ferrobot.theme", "dark"],
      ["ferrobot.accent", "teal"],
      ["ferrobot.bubbles", "huge"],
      ["ferrobot.text-size", "large"],
    ]);
    assert.deepEqual(
      readAppearance((k) => stored.get(k) ?? null),
      { ...DEFAULT_APPEARANCE, theme: "dark", accent: "teal", textSize: "large" },
    );
  });

  it("keeps every accent readable with the theme's accent text and as text on surfaces", () => {
    const lum = (hex: string): number => {
      const [r = 0, g = 0, b = 0] = [1, 3, 5].map((i) => {
        const v = parseInt(hex.slice(i, i + 2), 16) / 255;
        return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
      });
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    const ratio = (a: string, b: string): number => {
      const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x);
      return ((hi ?? 0) + 0.05) / ((lo ?? 0) + 0.05);
    };
    // Accent text and surface from styles.css: light, then dark.
    const modes = [
      { colors: "light", text: "#ffffff", surface: "#ffffff" },
      { colors: "dark", text: "#10131f", surface: "#1d1f24" },
    ] as const;
    for (const [id, accent] of Object.entries(ACCENTS)) {
      for (const m of modes) {
        for (const c of accent[m.colors]) {
          assert.ok(ratio(c, m.text) >= 4.5, `${id} ${m.colors} ${c} on accent text`);
          assert.ok(ratio(c, m.surface) >= 4.5, `${id} ${m.colors} ${c} on surface`);
        }
      }
    }
  });

  it("keeps the default accent equal to the theme's own tokens", async () => {
    const { readFile } = await import("node:fs/promises");
    const css = await readFile(new URL("../src/styles.css", import.meta.url), "utf8");
    for (const c of [...ACCENTS.blue.light, ...ACCENTS.blue.dark]) {
      assert.ok(css.includes(`--accent: ${c};`) || css.includes(`--accent-strong: ${c};`), c);
    }
  });
});

describe("shortcuts", () => {
  const press = (key: string, mods: Partial<KeyPress> = {}): KeyPress => ({
    key,
    code: /^[0-9]$/.test(key) ? `Digit${key}` : `Key${key.toUpperCase()}`,
    metaKey: false,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    repeat: false,
    isComposing: false,
    ...mods,
  });
  const cmd = { metaKey: true };
  const ctrl = { ctrlKey: true };

  it("matches ⌘ on macOS and Ctrl elsewhere", () => {
    assert.deepEqual(matchShortcut(press("k", cmd), true), { id: "switcher" });
    assert.deepEqual(matchShortcut(press("k", ctrl), false), { id: "switcher" });
    assert.equal(matchShortcut(press("k", ctrl), true), null);
    assert.equal(matchShortcut(press("k", cmd), false), null);
    assert.equal(matchShortcut(press("k", { ...cmd, ctrlKey: true }), true), null);
  });

  it("leaves plain typing alone; only Esc works without a modifier", () => {
    for (const key of ["k", "n", ",", "/", "1", "i"])
      assert.equal(matchShortcut(press(key), true), null);
    assert.deepEqual(matchShortcut(press("Escape"), true), { id: "close" });
    assert.equal(matchShortcut(press("k", { ...cmd, altKey: true }), true), null);
    assert.equal(matchShortcut(press("k", { ...cmd, isComposing: true }), true), null);
    assert.equal(matchShortcut(press("k", { ...cmd, repeat: true }), true), null);
  });

  it("checks Shift on letters, ignores it on punctuation, and maps digits by position", () => {
    assert.deepEqual(matchShortcut(press("U", { ...cmd, shiftKey: true }), true), { id: "usage" });
    assert.equal(matchShortcut(press("u", cmd), true), null);
    assert.equal(matchShortcut(press("K", { ...cmd, shiftKey: true }), true), null);
    assert.deepEqual(matchShortcut(press(",", cmd), true), { id: "settings" });
    // German layout: "/" is Shift+7.
    assert.deepEqual(matchShortcut({ ...press("/", cmd), code: "Digit7", shiftKey: true }, true), {
      id: "help",
    });
    assert.deepEqual(matchShortcut(press("3", cmd), true), { id: "agent", n: 3 });
    assert.deepEqual(matchShortcut({ ...press("&", ctrl), code: "Digit1" }, false), {
      id: "agent",
      n: 1,
    });
    assert.equal(matchShortcut(press("0", cmd), true), null);
  });

  it("writes keys the platform's way", () => {
    const usage = SHORTCUTS.find((s) => s.id === "usage");
    const agent = SHORTCUTS.find((s) => s.id === "agent");
    assert.ok(usage && agent);
    assert.equal(keyLabel(usage, true), "⌘⇧U");
    assert.equal(keyLabel(usage, false), "Ctrl+Shift+U");
    assert.equal(keyLabel(agent, true), "⌘1–9");
  });
});
