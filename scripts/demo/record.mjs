// The demo scenes: the README hero (and its X and LinkedIn cuts) plus short feature clips. Each run
// records one scene against the daemon record.sh points at, keeps take.json next to the frames, and
// encodes; `node record.mjs encode <scene> <frames dir>` re-encodes a kept take.
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { api, encode, studio, WAIT_MS } from "./lib.mjs";

const PROVIDER = process.env.DEMO_PROVIDER ?? "anthropic";
const MODEL = process.env.DEMO_MODEL ?? "sonnet";
const GROUP = "Launch crew";
const REACT =
  "When your operator writes to you in your own chat, first react to their message with one fitting emoji (the react tool, with its Message #id). Never react to [ferrobot] notes.";

const RESEARCHER = `You research and write for the team. Keep chat replies to one or two short sentences.
- In a group chat, answer in one short sentence. Asked what you take on: the release notes.
- ${REACT}
- Asked for release notes from an attached file: read it, write notes.html (one self-contained page: a title, three short sections with two bullets each, light styling, nothing external), share it with share_file, then reply in one sentence that names notes.html.`;

const BUILDER = `You build small web things. Keep chat replies to one short sentence.
- In a group chat, answer in one short sentence. Asked what you take on: the launch page. Asked for a diagram: one short sentence, then a \`\`\`mermaid block with a flowchart LR of at most six nodes with short labels.
- ${REACT}
- Asked for a widget: reply with one short sentence and one \`\`\`html block, nothing else: a self-contained page under 50 lines with inline <style> and <script> that fills the frame (html and body: margin 0, height 100vh) with its own dark background and large centred text, which the script updates every second. Do not write files or run commands for it.`;

const TEAM = [
  ["researcher", "Researches and writes the notes", RESEARCHER, ["docs", "release"]],
  ["builder", "Builds the launch page", BUILDER, ["web", "frontend"]],
];

// The file attached with the paperclip.
const CHANGELOG = `# ferrobot 0.1.5
- Group chats: write once and every agent answers in turn; @ one agent to ask only it.
- Reactions: agents react to your messages with an emoji, and you to theirs.
- Richer chat: Mermaid diagrams, live HTML previews, shared pages preview under their card.
- Attach files with the paperclip, drag and drop or paste.
- Tags, a model switch and a context ring for every agent.
- Plan limits of Claude Code and Codex sign-ins on the Usage page.
- Settings, Appearance: accent colour, bubbles, text size. Cmd+K jumps anywhere.
`;

const CUTS = {
  hero: [
    {
      name: "hero",
      mp4: "docs/assets/demo.mp4",
      width: 1280,
      holdMax: 0.9,
      waitMax: 0.8,
      gif: { file: "docs/assets/demo.gif", width: 800, fps: 6 },
    },
    {
      name: "x",
      mp4: "docs/launch/media/demo-x.mp4",
      width: 1280,
      holdMax: 0.9,
      waitMax: 0.8,
      skip: ["appearance"],
    },
    {
      name: "linkedin",
      mp4: "docs/launch/media/demo-linkedin.mp4",
      width: 1280,
      holdMax: 3,
      waitMax: 2.5,
    },
  ],
};
for (const [clip, file] of [
  ["keycard", "key-card"],
  ["routine", "routine"],
  ["memory", "memory"],
  ["usage", "usage-csv"],
]) {
  CUTS[clip] = [
    {
      name: clip,
      mp4: `docs/assets/clips/${file}.mp4`,
      width: 960,
      crf: 27,
      holdMax: 1,
      waitMax: 0.8,
      // The card says what was asked, so these clips start at the card.
      skip: ["ask"],
    },
  ];
}

const agents = async () => api("GET", "/v1/agents");
const agentNamed = async (name) => (await agents()).find((a) => a.name === name);
async function removeAgents(names) {
  for (const a of await agents())
    if (names.includes(a.name)) await api("DELETE", `/v1/agents/${a.id}`);
}
async function connected(name) {
  for (let i = 0; i < 600; i++) {
    if ((await agentNamed(name))?.connected) return;
    await sleep(500);
  }
  throw new Error(`${name} did not connect`);
}
async function finished(name) {
  for (let i = 0; i < WAIT_MS / 500; i++) {
    const a = await agentNamed(name);
    if (a && a.presence !== "working" && a.presence !== "starting") return;
    await sleep(500);
  }
}
/** Waits until a new agent's creation greeting turn has ended. */
async function greeted(name) {
  const { id } = await agentNamed(name);
  for (let i = 0; i < WAIT_MS / 500; i++) {
    const events = await api("GET", `/v1/agents/${id}/events`);
    if (events.some((e) => e.kind === "assistant_text")) break;
    await sleep(500);
  }
  await finished(name);
}
/** The provider DEMO_PROVIDER names; none (the mock) only for keyless dry runs. */
async function demoProvider() {
  const provider = (await api("GET", "/v1/providers")).find((p) => p.name === PROVIDER);
  if (!provider && process.env.DEMO_PROVIDER) throw new Error(`no provider named ${PROVIDER}`);
  return provider;
}
/** Waits until each of `names` said something in the group after line `from`, and nobody holds the floor. */
async function answered(group, from, names) {
  const ids = await Promise.all(names.map(async (n) => (await agentNamed(n)).id));
  for (let i = 0; i < WAIT_MS / 500; i++) {
    const room = await api("GET", `/v1/rooms/${String(group)}`);
    const spoke = room.lines.slice(from).map((l) => l.speaker.id);
    if (!room.speaking && ids.every((id) => spoke.includes(id))) return;
    await sleep(500);
  }
}

/** Locators and moves shared by the scenes. */
function moves(k) {
  const page = () => k.page;
  const row = (name) =>
    page()
      .locator(`button.agent-row[title^="${name} ·"], button.pinned-agent[title^="${name} ·"]`)
      .first();
  const chat = () => page().locator("section.chat");
  // Tool calls live on the info panel's Activities tab, newest first.
  const tool = (name, summary) => {
    let cards = page()
      .locator("aside.info .tool-card.t-ok")
      .filter({ has: page().locator(".tool-name", { hasText: name }) });
    if (summary)
      cards = cards.filter({ has: page().locator(".tool-summary", { hasText: summary }) });
    return cards;
  };
  return {
    row,
    chat,
    tool,
    activities: async () => {
      if (!(await page().locator("aside.info").isVisible())) {
        await k.click(chat().getByRole("button", { name: "Show details" }));
      }
      await k.click(page().getByRole("tab", { name: "Activities" }));
    },
    open: async (name) => {
      await k.click(row(name));
      await page().locator(`section.chat[aria-label="Chat with ${name}"]`).waitFor();
    },
    say: async (text, delay) => {
      const box = chat().locator("form.composer textarea");
      await k.zoom(box, 1.5);
      await k.click(box);
      await k.type(box, text, delay);
      await k.hold(300);
      await k.click(chat().getByLabel("Send"));
      await k.zoom(null);
    },
    /** Opens a chat or settings page through ⌘K. */
    jump: async (query, ready) => {
      await page().keyboard.press("ControlOrMeta+k");
      const box = page().getByRole("combobox", { name: "Agent or settings page" });
      await k.type(box, query, 110);
      await k.hold(500);
      await box.press("Enter");
      await page().locator(ready).waitFor();
    },
  };
}

async function hero(k) {
  const m = moves(k);
  const page = () => k.page;
  await page().emulateMedia({ colorScheme: "dark" });
  await removeAgents(["researcher", "builder"]);
  for (const g of await api("GET", "/v1/groups"))
    if (g.name === GROUP) await api("DELETE", `/v1/groups/${String(g.id)}`);
  await api("PUT", "/v1/settings", { ...(await api("GET", "/v1/settings")), auto_mode: false });
  const provider = await demoProvider();
  for (const [name, description, instructions, tags] of TEAM)
    await api("POST", "/v1/agents", {
      name,
      description,
      instructions,
      tags,
      ...(provider ? { provider_id: provider.id, model: MODEL } : {}),
    });
  for (const [name] of TEAM) {
    await connected(name);
    await greeted(name);
    await api("POST", `/v1/agents/${(await agentNamed(name)).id}/read`);
  }
  const changelog = join(process.env.DEMO_FRAMES ?? "", "changelog.md");
  writeFileSync(changelog, CHANGELOG);

  await k.start();
  await k.caption("A team of agents, each with its own computer");
  await k.zoom(page().locator(".sidebar"), 1.5);
  await k.hold(1600);
  await k.zoom(null);

  await k.caption("Group chats: write once, the whole team answers");
  await k.click(page().getByRole("button", { name: "New group" }));
  const dialog = page().locator('.dialog[aria-label="New group"]');
  await k.type(dialog.getByPlaceholder("e.g. Release team"), GROUP, 45);
  for (const [name] of TEAM) await k.click(dialog.locator("label.check", { hasText: name }));
  await k.click(dialog.getByRole("button", { name: "Create" }));
  await page().locator(`section.chat[aria-label="${GROUP}"]`).waitFor();
  const group = (await api("GET", "/v1/groups")).find((g) => g.name === GROUP).id;
  const lines = async () => (await api("GET", `/v1/rooms/${String(group)}`)).lines.length;

  let from = await lines();
  await m.say("Morning, team! 0.1.5 ships on Friday. One line each: what do you take on?", 16);
  await k.wait(() => answered(group, from, ["researcher", "builder"]));
  await k.zoom(m.chat().locator(".timeline"), 1.3);
  await k.hold(1500);
  await k.zoom(null);

  await k.caption("@ one agent, and only that one answers");
  const box = m.chat().locator("form.composer textarea");
  await k.zoom(box, 1.5);
  await k.click(box);
  await k.type(box, "@b", 140);
  await k.hold(700);
  await k.click(m.chat().locator('.mention-pick [role="option"]', { hasText: "builder" }));
  await k.type(box, "sketch the release flow as a diagram", 16);
  await k.hold(300);
  from = await lines();
  await k.click(m.chat().getByLabel("Send"));
  await k.zoom(m.chat().locator(".row-user").last(), 1.5);
  await k.hold(1200);
  await k.zoom(null);
  await k.caption("Mermaid diagrams draw right in the chat");
  await k.wait(async () => {
    await answered(group, from, ["builder"]);
    await m.chat().locator("img.mermaid-img").last().waitFor({ timeout: WAIT_MS });
  });
  await k.zoom(m.chat().locator(".row-agent").last(), 1.35);
  await k.hold(2200);
  await k.zoom(null);

  await m.open("researcher");
  await k.caption("Tags, the model and how full its context is, at a glance");
  await k.zoom(m.chat().locator(".chat-name"), 2);
  await k.hold(1400);
  await k.zoom(m.chat().locator(".ctx-ring"), 2);
  await k.hold(1100);
  await k.zoom(null);
  await k.caption("Attach files with the paperclip");
  const chooser = page().waitForEvent("filechooser");
  await k.click(m.chat().getByLabel("Attach files"));
  await (await chooser).setFiles(changelog);
  await m.say("Turn this into release notes and share them as a page.", 16);

  await k.caption("⌘K jumps to any agent");
  await m.jump("bui", 'section.chat[aria-label="Chat with builder"]');
  await k.caption(null);
  await m.say("Make a live countdown to Friday's launch, Nov 27 at 14:00 UTC.", 16);
  await k.caption("Agents react to your messages…");
  const mine = m.chat().locator(".row-user").last();
  await k.wait(mine.locator(".reaction"));
  await k.zoom(mine, 1.6);
  await k.hold(1100);
  await k.zoom(null);
  await k.caption("…and answer with live HTML: styled, and its script runs");
  const frame = m.chat().locator("iframe.block-frame").last();
  await k.wait(async () => {
    await finished("builder");
    await frame.waitFor({ timeout: WAIT_MS });
  });
  await k.zoom(frame, 1.35);
  await k.hold(2600);
  await k.zoom(null);

  await k.caption("…and you react to theirs");
  const reply = m.chat().locator(".bubble-agent").last();
  await reply.hover();
  await k.click(reply.getByRole("button", { name: "React" }));
  await k.click(reply.getByRole("menuitemradio", { name: "🎉" }));
  await k.zoom(reply.locator(".msg-foot"), 2);
  await k.hold(1000);
  await k.zoom(null);

  await k.wait(() => finished("researcher"));
  await k.caption("A New line marks what you missed");
  await m.open("researcher");
  const divider = m.chat().locator(".new-divider");
  await divider.scrollIntoViewIfNeeded({ timeout: 5000 }).catch(() => undefined);
  await k.zoom(divider, 1.5);
  await k.hold(1300);
  await k.zoom(null);
  await k.caption("Shared pages preview right under their card");
  const shared = m.chat().locator(".file-share").last();
  await shared.scrollIntoViewIfNeeded({ timeout: 5000 }).catch(() => undefined);
  await k.zoom(shared, 1.3);
  await k.hold(2400);
  await k.zoom(null);

  // Reading a plan's limits runs the host's CLI for a few seconds; the daemon keeps them a minute.
  await k.offscreen(() => api("GET", "/v1/limits"));
  await k.caption("Your Claude plan's 5-hour and weekly limits, on Usage");
  await k.click(page().getByRole("button", { name: "Usage", exact: true }));
  const limits = page().locator(".usage-limits");
  await k.wait(limits.locator(".usage-ring").first());
  await k.zoom(limits, 2.2);
  await k.hold(2200);
  await k.zoom(null);

  await k.optional("appearance", async () => {
    await k.caption("Make it yours: ⌘K, Appearance, an accent colour");
    await m.jump("appear", '.swatches[aria-label="Accent color"]');
    await k.zoom(page().locator(".pref-list"), 1.3);
    await k.click(page().getByRole("radio", { name: "Pink" }));
    await k.hold(1400);
    await k.zoom(null);
    await k.click(page().getByRole("button", { name: "Close settings" }));
    await k.click(page().locator('section[aria-label="Groups"] button.convo-row'));
    await k.caption(null);
    await k.hold(1800);
  });
  await k.caption(null);
  await k.hold(600);
}

/** The clips start in researcher's chat; the hero leaves researcher behind, else it is created. */
async function researcherChat(k) {
  if (!(await agentNamed("researcher"))) {
    const provider = (await api("GET", "/v1/providers")).find((p) => p.name === PROVIDER);
    await api("POST", "/v1/agents", {
      name: "researcher",
      description: "Finds answers in the docs",
      instructions: RESEARCHER,
      ...(provider ? { provider_id: provider.id } : {}),
    });
    await connected("researcher");
  }
  const m = moves(k);
  await m.open("researcher");
  return m;
}

// A reserved name (RFC 2606): no vendor, and the connection check fails honestly.
const DOCS_URL = "https://mcp.example.com/mcp";

async function keycard(k) {
  for (const c of await api("GET", "/v1/connectors")) {
    if (c.url === DOCS_URL) await api("DELETE", `/v1/connectors/${c.id}`);
  }
  const m = await researcherChat(k);
  const page = k.page;
  await k.start();
  await k.caption("An agent asks for a token for a new connector");
  const card = m.chat().locator("form.qcard").last();
  await k.optional("ask", async () => {
    await m.say(`Add our docs search connector at ${DOCS_URL}. It needs a token.`, 18);
    await k.wait(card);
  });
  await k.zoom(card, 1.4);
  await k.hold(1500);
  await k.caption("You type it into a hidden field");
  await k.click(card.getByRole("radio", { name: "Approve" }));
  const field = card.locator(".qcard-secret input");
  await k.click(field);
  await k.type(field, "demo-token-not-real", 45);
  await k.hold(500);
  await k.click(card.getByRole("button", { name: "Send answers" }));
  await k.zoom(null);
  await k.caption("It goes straight to Vault; the agent never sees it");
  await k.click(page.getByRole("button", { name: "Settings", exact: true }));
  await k.click(page.locator("button.settings-tab", { hasText: "Vault" }));
  const section = page.locator(".settings-section", { hasText: "Connectors" });
  await section.waitFor();
  await k.zoom(section, 1.4);
  await k.hold(2800);
  await k.offscreen(async () => {
    await k.zoom(null);
    await page.getByRole("button", { name: "Close settings" }).click();
    await finished("researcher");
  });
}

async function routine(k) {
  const m = await researcherChat(k);
  const page = k.page;
  await k.start();
  await k.caption("Ask an agent for a reminder");
  const card = m.chat().locator("form.qcard").last();
  await k.optional("ask", async () => {
    await m.say("Remind me every morning at 9 to check the build.", 25);
    await k.wait(card);
  });
  await k.caption("It asks before it sets up a routine");
  await k.zoom(card, 1.4);
  await k.hold(1800);
  await k.click(card.getByRole("radio", { name: "Approve" }));
  await k.click(card.getByRole("button", { name: "Send answers" }));
  await k.zoom(null);
  await k.caption("The routine is in its Routines tab");
  await k.click(m.chat().getByRole("button", { name: "Show details" }));
  await k.click(page.getByRole("tab", { name: "Routines" }));
  const list = page.locator("aside.info");
  await k.wait(list.getByRole("button", { name: "Run now" }).first());
  await k.zoom(list, 1.3);
  await k.hold(2800);
  await k.offscreen(async () => {
    const { id } = await agentNamed("researcher");
    for (const r of await api("GET", `/v1/agents/${id}/routines`)) {
      await api("DELETE", `/v1/routines/${String(r.id)}`);
    }
    await k.zoom(null);
    await page.getByRole("button", { name: "Hide details" }).click();
    await finished("researcher");
  });
}

/** Still screenshots for docs/screenshots, light and dark (1x, no cursor or captions). */
async function shots(k) {
  const page = k.page;
  await page.addStyleTag({
    content: "#demo-cursor, #demo-ring, #demo-cap, #demo-badge { display: none !important; }",
  });
  const snap = async (name) => {
    for (const scheme of ["light", "dark"]) {
      await page.emulateMedia({ colorScheme: scheme });
      await page.waitForTimeout(500);
      await page.screenshot({ path: `docs/screenshots/${scheme}-${name}.png`, scale: "css" });
    }
    await page.emulateMedia({ colorScheme: "light" });
  };
  const m = await researcherChat(k);
  const composer = m.chat().locator("form.composer textarea");
  // It already knows the runtime from the hero, so it is asked to check with you again.
  await composer.fill(
    "Next we need a retry example for the CLI tool. Ask me which runtime it uses before you brief builder.",
  );
  await composer.press("Enter");
  await m.chat().locator("form.qcard").last().waitFor({ timeout: WAIT_MS });
  await finished("researcher");
  await snap("question-card");

  const team = (await agents()).filter((a) => ["researcher", "builder"].includes(a.name));
  if (team.length === 2) {
    await api("PUT", "/v1/call", { agents: team.map((a) => a.id) });
    await api("POST", "/v1/call/messages", {
      body: "Quick check before we ship: does the retry example still build?",
    });
    for (let i = 0; i < WAIT_MS / 1000; i++) {
      const [room] = await api("GET", "/v1/rooms");
      const lines = room ? (await api("GET", `/v1/rooms/${String(room.id)}`)).lines : [];
      if (lines.filter((l) => l.speaker.kind === "agent").length >= 2) break;
      await sleep(1000);
    }
    for (const a of team) await finished(a.name);
    await page.locator('section[aria-label="Calls"] button.convo-row').first().click();
    await page.waitForTimeout(800);
    await snap("call-room");
    await api("PUT", "/v1/call", { agents: [] });
  }

  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.locator("button.settings-tab", { hasText: "Vault" }).click();
  await page.locator(".settings-section").first().waitFor();
  await snap("vault");
  await page.getByRole("button", { name: "Close settings" }).click();

  await page.getByRole("button", { name: "Usage", exact: true }).click();
  await page.locator(".chips button", { hasText: "Today" }).click();
  await page.locator(".usage-page .usage-totals").waitFor();
  await snap("usage");
}

// Something only memory holds: a question its chat could answer would skip `recall`.
const RELEASE_DAY = "Releases go out on Thursdays at 14:00 UTC, never right before a holiday.";

async function memory(k) {
  const m = await researcherChat(k);
  const id = (await agentNamed("researcher")).id;
  await api("POST", `/v1/agents/${id}/memory`, { key: "release-day", content: RELEASE_DAY });
  await k.start();
  await k.caption("Agents remember, and recall it when it matters");
  await m.say("Check your notes: when do we ship releases?", 25);
  await m.activities();
  const recall = m.tool("recall").first();
  await k.wait(recall);
  await k.zoom(recall, 1.4);
  await k.hold(1200);
  await k.wait(() => finished("researcher"));
  await k.zoom(m.chat().locator(".bubble-agent").last(), 1.4);
  await k.hold(2500);
}

async function usage(k) {
  const page = k.page;
  await page.getByRole("button", { name: "Usage", exact: true }).click();
  await page.locator(".chips button", { hasText: "Today" }).click();
  await page.locator(".usage-page .usage-totals").waitFor();
  await k.start();
  await k.caption("Usage per provider, model and agent, as CSV");
  await k.pause(1500);
  const download = page.waitForEvent("download");
  await k.click(page.getByRole("button", { name: "Export CSV" }));
  const file = await download;
  const csv = readFileSync(await file.path(), "utf8").replace(/[&<]/g, (c) =>
    c === "&" ? "&amp;" : "&lt;",
  );
  const sheet = await page.context().newPage();
  await sheet.setContent(
    `<pre style="font: 22px/1.6 ui-monospace, Menlo, monospace; margin: 120px 80px; color: #1f2330; white-space: pre-wrap">${csv}</pre>`,
  );
  await k.follow(sheet);
  await k.caption(`${file.suggestedFilename()}, as exported`);
  await k.pause(3000);
}

const SCENES = { hero, keycard, routine, memory, usage, shots };

async function main() {
  const [scene, encodeScene, encodeDir] = process.argv.slice(2);
  if (scene === "encode") {
    const take = JSON.parse(readFileSync(join(encodeDir ?? "", "take.json"), "utf8"));
    for (const cut of CUTS[encodeScene ?? ""] ?? []) encode(take, encodeDir ?? "", cut);
    return;
  }
  const run = SCENES[scene ?? ""];
  if (!run)
    throw new Error(`unknown scene ${scene ?? ""}; known: ${Object.keys(SCENES).join(", ")}`);
  const dir = process.env.DEMO_FRAMES ?? "";
  const k = await studio(dir);
  let take;
  try {
    await run(k);
  } finally {
    await k.page.screenshot({ path: join(dir, "last.png") }).catch(() => undefined);
    take = await k.finish();
    writeFileSync(join(dir, "take.json"), JSON.stringify(take));
  }
  for (const cut of CUTS[scene] ?? []) encode(take, dir, cut);
}

await main();
