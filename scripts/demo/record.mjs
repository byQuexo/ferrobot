// The demo scenes: the README hero (and its X and LinkedIn cuts) plus short feature clips. Each run
// records one scene against the daemon record.sh points at, keeps take.json next to the frames, and
// encodes; `node record.mjs encode <scene> <frames dir>` re-encodes a kept take.
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { api, encode, studio, WAIT_MS } from "./lib.mjs";

const PROVIDER = process.env.DEMO_PROVIDER ?? "anthropic";
const ASK =
  "Find out how to do retries with backoff in Rust using the backon crate, then have builder write a working example.";
const ENV_CHECK = "env | grep -E 'TOKEN|KEY|URL' | sed -E 's/=.*/=•••/'";

const RESEARCHER = `You research how to do things and brief your teammates. Keep chat replies to one or two short sentences.
- Read library docs with the deepwiki connector tools; the backon crate lives in the Xuanwo/backon repo. One or two questions are enough.
- Before you hand off code, ask the operator one question with ask_user: prompt "Async (tokio) or blocking?", choices "Async (tokio)" and "Blocking". Then end your turn.
- When the answer arrives, give the build to builder with assign_task: a small example where an operation fails twice and succeeds on the third try, retried with backon's ExponentialBuilder, printing each attempt. Name the dependencies: backon = "1", and for async tokio = { version = "1", features = ["full"] }.
- When builder's answer arrives, write report.html: one small self-contained page with a title, three sentences on how backon retries work, the code and the program's output, light styling. Share it with share_file, then reply in one sentence.`;

const BUILDER = `You write and run code on your own computer. Keep chat replies short.
Rust is installed: start every shell command that uses cargo with \`. ~/.cargo/env && \`.
For a coding task: create the project in a new folder in your workspace, write Cargo.toml (edition 2021, exactly the dependency versions you are given) and src/main.rs, run cargo run, and fix errors until it works.
After it runs, show the operator which credentials your computer holds: run exactly this as its own command, nothing chained: ${ENV_CHECK}
Answer with the code and the program's output, nothing else.`;

const CUTS = {
  hero: [
    {
      name: "hero",
      mp4: "docs/assets/demo.mp4",
      width: 1280,
      holdMax: 0.9,
      waitMax: 0.8,
      skip: ["builder-create"],
      gif: { file: "docs/assets/demo.gif", width: 800, fps: 8 },
    },
    {
      name: "x",
      mp4: "docs/launch/media/demo-x.mp4",
      width: 1280,
      holdMax: 0.9,
      waitMax: 0.8,
      skip: ["create", "builder-create"],
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
async function ensureClis() {
  const want = ["gcc", "libc6-dev"];
  let clis = await api("GET", "/v1/clis");
  if (want.some((p) => !clis.packages.includes(p))) {
    clis = await api("PUT", "/v1/clis", { packages: [...new Set([...clis.packages, ...want])] });
  }
  while (clis.status === "building") {
    await sleep(2000);
    clis = await api("GET", "/v1/clis");
  }
  if (clis.status !== "ready") throw new Error(`CLI image: ${clis.status} ${clis.error ?? ""}`);
}
async function exec(name, script) {
  const a = await agentNamed(name);
  const out = await api("POST", `/v1/agents/${a.id}/exec`, { cmd: ["bash", "-c", script] });
  if (out.exit_code !== 0) throw new Error(`${name}: ${out.stderr.slice(-800)}`);
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
    /** Opens a tool card and zooms on its result. */
    expand: async (card) => {
      await k.click(card.locator(".tool-head"));
      await k.zoom(card.locator(".tool-body pre").last(), 1.35);
    },
    newAgent: async (name, description, switches = []) => {
      await k.click(page().locator('.sidebar .brand [aria-label="New agent"]'));
      const dialog = page().locator('.dialog[aria-label="New agent"]');
      await k.type(dialog.getByPlaceholder("e.g. researcher"), name, 45);
      await k.type(dialog.getByPlaceholder("One line shown in the agent list"), description, 25);
      for (const label of switches) {
        await k.click(dialog.getByRole("switch", { name: label, exact: true }));
      }
      await k.click(dialog.getByRole("button", { name: "Create agent" }));
      await page().locator(`section.chat[aria-label="Chat with ${name}"]`).waitFor();
    },
  };
}

async function hero(k) {
  const m = moves(k);
  const page = () => k.page;
  await page().emulateMedia({ colorScheme: "dark" });
  await removeAgents(["researcher", "builder"]);
  await api("PUT", "/v1/settings", { ...(await api("GET", "/v1/settings")), auto_mode: false });
  await ensureClis();
  if (!(await api("GET", "/v1/connectors")).some((c) => c.name === "deepwiki")) {
    await page().getByRole("button", { name: "Settings", exact: true }).click();
    await page().locator("button.settings-tab", { hasText: "Connectors" }).click();
    await page().getByRole("button", { name: "Add DeepWiki" }).click();
    await page().locator(".provider-row", { hasText: "connected" }).waitFor({ timeout: 60_000 });
    await page().getByRole("button", { name: "Close settings" }).click();
  }

  await k.start();
  await k.optional("create", async () => {
    await k.hold(600);
    await k.caption("Two agents, each with its own computer");
    await m.newAgent("researcher", "Finds answers in the docs", ["deepwiki", "Web"]);
  });
  await k.optional("builder-create", () =>
    k.fast(() => m.newAgent("builder", "Writes and runs code")),
  );
  await k.offscreen(async () => {
    const provider = (await api("GET", "/v1/providers")).find((p) => p.name === PROVIDER);
    for (const [name, instructions] of [
      ["researcher", RESEARCHER],
      ["builder", BUILDER],
    ]) {
      const a = await agentNamed(name);
      if (provider && a.provider_id !== provider.id)
        throw new Error(`${name} is not on ${PROVIDER}`);
      await api("PATCH", `/v1/agents/${a.id}`, { instructions });
      await connected(name);
    }
    await exec("builder", readFileSync(new URL("warm-rust.sh", import.meta.url), "utf8"));
    // A running turn keeps its prompt: the ask must not land in the greeting turn, before the role.
    for (const name of ["researcher", "builder"]) await greeted(name);
    await m.open("researcher");
    await m.activities();
    await k.caption(null);
  });

  await k.caption("Ask researcher");
  await m.say(ASK, 16);
  await k.caption("It reads the backon docs through the DeepWiki connector");
  const wiki = m.tool("deepwiki__").first();
  await k.wait(wiki);
  await k.zoom(wiki, 1.35);
  await k.hold(1300);

  const card = m.chat().locator("form.qcard").first();
  await k.wait(card);
  await k.caption("…and asks you when it matters");
  await k.zoom(card, 1.45);
  await k.hold(900);
  await k.click(card.getByRole("radio", { name: /tokio/i }));
  await k.click(card.getByRole("button", { name: "Send answers" }));

  await k.caption("It hands the build to builder");
  const handoff = m.tool("assign_task").first();
  await k.wait(handoff);
  await k.zoom(handoff, 1.4);
  await k.hold(1000);
  await k.zoom(null);

  await m.open("builder");
  await k.caption("builder writes the code and runs cargo on its own computer");
  const envCard = m.tool("Bash", "env | grep").first();
  await k.wait(envCard);
  const run = m.tool("Bash", "cargo run").first();
  await m.expand(run);
  await k.hold(2000);
  // Click at zoom 1: a target outside the zoomed view makes Playwright scroll under the transform.
  await k.zoom(null);
  await k.caption(
    "ANTHROPIC_API_KEY here is the agent's own token. The real key never enters the sandbox.",
  );
  await m.expand(envCard);
  await k.pause(3000);
  await k.zoom(null);

  await m.open("researcher");
  await k.caption("builder reports back; researcher shares the report");
  const file = m.chat().locator("button.file-card").last();
  await k.wait(async () => {
    await file.waitFor({ timeout: WAIT_MS });
    await finished("researcher");
  });
  await k.zoom(m.chat().locator(".bubble-agent").last(), 1.4);
  await k.hold(1300);
  await k.zoom(null);
  const app = page();
  const popup = app.context().waitForEvent("page");
  await k.click(file);
  await k.follow(await popup);
  await k.caption("The report opens from a signed share link");
  await k.hold(2500);
  await k.page.mouse.wheel(0, 500);
  await k.hold(1200);
  const share = k.page;
  await k.follow(app);
  await share.close();

  await k.caption("Every model call is counted, with its estimated cost");
  await k.click(app.getByRole("button", { name: "Usage", exact: true }));
  await k.click(app.locator(".chips button", { hasText: "Today" }));
  await k.zoom(app.locator(".usage-page .usage-totals"), 1.4);
  await k.hold(2500);
  await k.zoom(null);
  await k.caption(null);
  await k.hold(800);
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
