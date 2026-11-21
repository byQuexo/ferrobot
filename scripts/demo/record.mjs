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
After it runs, show the operator which credentials your computer holds, with exactly this command: ${ENV_CHECK}
Answer with the code and the program's output, nothing else.`;

const CUTS = {
  hero: [
    {
      name: "hero",
      mp4: "docs/assets/demo.mp4",
      width: 1280,
      holdMax: 1.4,
      waitMax: 1.2,
      skip: ["builder-create"],
      gif: { file: "docs/assets/demo.gif", width: 800, fps: 9 },
    },
    {
      name: "x",
      mp4: "docs/launch/media/demo-x.mp4",
      width: 1280,
      holdMax: 1.4,
      waitMax: 1.2,
      skip: ["builder-create"],
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
for (const clip of ["connector", "routine", "memory", "usage"]) {
  CUTS[clip] = [
    {
      name: clip,
      mp4: `docs/assets/clips/${clip}.mp4`,
      width: 960,
      crf: 27,
      holdMax: 2,
      waitMax: 1,
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
  const tool = (name, summary) => {
    let cards = chat()
      .locator(".tool-card.t-ok")
      .filter({ has: page().locator(".tool-name", { hasText: name }) });
    if (summary)
      cards = cards.filter({ has: page().locator(".tool-summary", { hasText: summary }) });
    return cards;
  };
  return {
    row,
    chat,
    tool,
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
    expand: async (card) => {
      await k.click(card.locator(".tool-head"));
      await k.zoom(card, 1.35);
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
  await k.hold(800);
  await k.caption("Two agents, each with its own computer");
  await m.newAgent("researcher", "Finds answers in the docs", ["deepwiki", "Web"]);
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
    await m.open("researcher");
    await k.caption(null);
  });

  await k.caption("Ask researcher");
  await m.say(ASK, 22);
  await k.caption("It reads the backon docs through the DeepWiki connector");
  const wiki = m.tool("deepwiki__").first();
  await k.wait(wiki);
  await k.zoom(wiki, 1.35);
  await k.hold(1800);
  await k.zoom(null);

  const card = m.chat().locator("form.qcard").first();
  await k.wait(card);
  await k.caption("…and asks you when it matters");
  await k.zoom(card, 1.5);
  await k.hold(1200);
  await k.click(card.getByRole("radio", { name: /tokio/i }));
  await k.click(card.getByRole("button", { name: "Send answers" }));
  await k.zoom(null);

  await k.caption("It hands the build to builder");
  const handoff = m.tool("assign_task").first();
  await k.wait(handoff);
  await k.zoom(handoff, 1.4);
  await k.hold(1500);
  await k.zoom(null);

  await m.open("builder");
  await k.caption("builder writes the code and runs cargo on its own computer");
  const envCard = m.tool("Bash", "env | grep").last();
  await k.wait(envCard);
  const run = m.tool("Bash", "cargo run").last();
  await m.expand(run);
  await k.hold(2500);
  await k.zoom(null);
  await k.click(run.locator(".tool-head"));
  await k.caption(
    "ANTHROPIC_API_KEY here is the agent's own token. The real key never enters the sandbox.",
  );
  await m.expand(envCard);
  await k.hold(3200);
  await k.zoom(null);

  await m.open("researcher");
  await k.caption("builder reports back; researcher shares the report");
  const file = m.chat().locator("button.file-card").last();
  await k.wait(async () => {
    await file.waitFor({ timeout: WAIT_MS });
    await finished("researcher");
  });
  await k.zoom(m.chat().locator(".bubble-agent").last(), 1.4);
  await k.hold(1800);
  await k.zoom(null);
  const app = page();
  const popup = app.context().waitForEvent("page");
  await k.click(file);
  await k.follow(await popup);
  await k.caption("The report opens from a signed share link");
  await k.hold(3500);
  await k.page.mouse.wheel(0, 500);
  await k.hold(1500);
  const share = k.page;
  await k.follow(app);
  await share.close();

  await k.caption("Every model call is counted, with its estimated cost");
  await k.click(app.getByRole("button", { name: "Usage", exact: true }));
  await k.click(app.locator(".chips button", { hasText: "Today" }));
  await k.zoom(app.locator(".usage-page .usage-totals"), 1.4);
  await k.hold(3000);
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

async function connector(k) {
  const m = await researcherChat(k);
  await k.start();
  await k.caption("Agents ask before they add a connector");
  await m.say(
    "Add the Linear connector (https://mcp.linear.app/mcp) so you can read our issues.",
    18,
  );
  const card = m.chat().locator("form.qcard").last();
  await k.wait(card);
  await k.zoom(card, 1.45);
  await k.hold(3500);
  await k.offscreen(async () => {
    await card.getByRole("radio", { name: "Decline" }).click();
    await card.getByRole("button", { name: "Send answers" }).click();
    await finished("researcher");
  });
}

async function routine(k) {
  const m = await researcherChat(k);
  const page = k.page;
  await k.start();
  await k.caption("Routines run a prompt on a schedule");
  await k.click(m.chat().getByRole("button", { name: "Show details" }));
  await k.click(page.getByRole("tab", { name: "Routines" }));
  await k.click(page.getByRole("button", { name: "New routine" }));
  const form = page.locator("aside.info form.card.form");
  await k.zoom(form, 1.35);
  await k.type(form.getByPlaceholder("morning-digest"), "backon-releases", 30);
  await k.click(form.locator(".chips button", { hasText: "Fridays at 18:00" }));
  await form.locator("label", { hasText: "Time zone" }).locator("input").fill("UTC");
  await k.type(
    form.getByPlaceholder("Summarize what changed since yesterday."),
    "Check backon for new releases and tell me what changed.",
    14,
  );
  await k.click(form.getByRole("button", { name: "Create routine" }));
  await k.zoom(null);
  await k.hold(2500);
  await k.offscreen(async () => {
    const rid = (await api("GET", `/v1/agents/${(await agentNamed("researcher")).id}/routines`))[0]
      ?.id;
    if (rid) await api("DELETE", `/v1/routines/${String(rid)}`);
    await page.getByRole("button", { name: "Hide details" }).click();
  });
}

const RETRY_POLICY =
  "Retries use backon's ExponentialBuilder: 100 ms first delay, at most 5 attempts, with jitter.";

async function memory(k) {
  const m = await researcherChat(k);
  const id = (await agentNamed("researcher")).id;
  await api("POST", `/v1/agents/${id}/memory`, { key: "retry-policy", content: RETRY_POLICY });
  await k.start();
  await k.caption("Agents remember, and recall it when it matters");
  await m.say("What retry policy did we settle on?", 25);
  const recall = m.tool("recall").last();
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
  await k.hold(1200);
  const download = page.waitForEvent("download");
  await k.click(page.getByRole("button", { name: "Export CSV" }));
  const file = await download;
  const csv = readFileSync(await file.path(), "utf8").replace(/[&<]/g, (c) =>
    c === "&" ? "&amp;" : "&lt;",
  );
  const sheet = await page.context().newPage();
  await sheet.setContent(
    `<pre style="font: 15px/1.6 ui-monospace, Menlo, monospace; margin: 40px; color: #1f2330">${csv}</pre>`,
  );
  await k.follow(sheet);
  await k.caption(`${file.suggestedFilename()}, as exported`);
  await k.hold(3000);
}

const SCENES = { hero, connector, routine, memory, usage };

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
  for (const cut of CUTS[scene]) encode(take, dir, cut);
}

await main();
