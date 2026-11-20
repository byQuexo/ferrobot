// Plays the demo story on the throwaway daemon that record.sh starts: sets up a provider (key on stdin),
// team memory and a lead agent, drives the UI with an on-screen cursor and zooms, and records CDP
// screencast frames into an ffconcat list whose waits for the model are sped up.
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const { DEMO_API: API, DEMO_UI: UI, DEMO_FRAMES: FRAMES } = process.env;
const TOKEN = readFileSync(process.env.DEMO_TOKEN_FILE ?? "", "utf8").trim();
const KIND = process.env.DEMO_PROVIDER_KIND ?? "openrouter";
const MODEL = process.env.DEMO_MODEL ?? "tencent/hy4-flash";
const TEAM = ["scout", "quill"];
const PROMPT = "Plan the v0.1 launch post: check what changed and draft it.";
const WAIT_MS = 240_000;

const NOTES = `ferrobot v0.1, what changed:
- Every agent gets its own computer: a sandboxed container with a home that persists.
- Agents message each other, hand out tasks and get the answers back automatically.
- Agents can create teammates; the operator approves each one with a card.
- Question cards: an agent asks, the operator answers with one click.
- Routines run tasks on a schedule; memory is shared across the team.
- Bring any model: Anthropic, OpenAI, OpenRouter, Ollama, Gemini or Codex. Keys never leave the daemon.`;

const LEAD_ROLE = `You lead launches for the ferrobot team. Keep replies short and plain: one or two sentences, no headings.
When asked for the v0.1 launch post:
1. Say in one sentence how you will split the work.
2. Call create_agent twice, one after the other:
   - name "scout", description "Research", instructions "Dig up facts fast and write them down.", task: "Recall the team memory release-v0.1. With bash, write its highlights as short bullets to notes/whats-new.md, then run ls -l notes and wc -l notes/whats-new.md. Answer with the highlights in five short lines."
   - name "quill", description "Writing", instructions "Write short, warm launch copy.", task: "Recall the team memory release-v0.1. With write_file, write a launch post of about 120 words to drafts/launch-post.md: a title line, two short paragraphs, one closing line. Then run wc -w drafts/launch-post.md with bash. Answer with the post."
3. End your turn; their answers come back to you on their own.
When a task answer arrives and you do not have both answers yet, reply in one short line and end your turn.
When you have both answers: write launch-post.md with quill's post, adding anything from scout's highlights it missed, call share_file with launch-post.md, then reply in two short sentences.`;

const require = createRequire(import.meta.url);
const root = execFileSync("npm", ["root", "-g"], { encoding: "utf8" }).trim();
const { chromium } = require(join(root, "@playwright/cli/node_modules/playwright"));

// shortcut: an older cached Chromium stands in when the CLI updated Playwright; `playwright install` fixes it.
function chromiumPath() {
  const expected = chromium.executablePath();
  const m = /^(.*[\\/])chromium-\d+([\\/].*)$/.exec(expected);
  if (existsSync(expected) || !m) return undefined;
  const builds = readdirSync(m[1]).filter((d) => /^chromium-\d+$/.test(d));
  builds.sort((a, b) => Number(b.slice(9)) - Number(a.slice(9)));
  return builds[0] ? `${m[1]}${builds[0]}${m[2]}` : undefined;
}

async function api(method, path, body) {
  const res = await fetch(`${API}${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${TOKEN}`,
      ...(body ? { "content-type": "application/json" } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`${method} ${path}: ${String(res.status)} ${await res.text()}`);
  return res.status === 204 ? null : res.json();
}

async function readKey() {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  const key = Buffer.concat(chunks).toString("utf8").trim();
  if (!key) throw new Error("pipe the provider's API key on stdin");
  return key;
}

async function setup() {
  const provider = await api("POST", "/v1/providers", {
    name: KIND,
    kind: KIND,
    default_model: MODEL,
    api_key: await readKey(),
    ...(process.env.DEMO_BASE_URL ? { base_url: process.env.DEMO_BASE_URL } : {}),
  });
  const settings = await api("GET", "/v1/settings");
  if (settings.auto_mode) await api("PUT", "/v1/settings", { ...settings, auto_mode: false });
  await api("POST", "/v1/memory", { key: "release-v0.1", content: NOTES });
  const lead = await api("POST", "/v1/agents", {
    name: "atlas",
    description: "Launch lead",
    instructions: LEAD_ROLE,
    provider_id: provider.id,
  });
  for (let i = 0; i < 300 && !(await api("GET", `/v1/agents/${lead.id}`)).connected; i++) {
    await sleep(500);
  }
}

// Runs in the page: a cursor and click ring outside #app, and a CSS zoom on #app.
function overlay() {
  let x = 900;
  let y = 700;
  let s = 1;
  let tx = 0;
  let ty = 0;
  const ensure = () => {
    if (document.getElementById("demo-cursor")) return;
    const style = document.createElement("style");
    style.textContent = `
      html, body { overflow: hidden !important; }
      #app { transform-origin: 0 0; transition: transform 950ms cubic-bezier(.45,.05,.2,1); }
      #demo-cursor, #demo-ring { position: fixed; left: 0; top: 0; pointer-events: none; z-index: 2147483647; }
      #demo-cursor { width: 26px; height: 26px; filter: drop-shadow(0 2px 3px rgba(0,0,0,.35)); }
      #demo-ring { width: 40px; height: 40px; margin: -20px 0 0 -20px; border-radius: 50%; opacity: 0;
        border: 2px solid rgba(79,70,229,.85); background: rgba(79,70,229,.18); }`;
    document.head.append(style);
    const ring = document.createElement("div");
    ring.id = "demo-ring";
    const cursor = document.createElement("div");
    cursor.id = "demo-cursor";
    cursor.innerHTML =
      '<svg viewBox="0 0 26 26" width="26" height="26"><path d="M3 2 L3 21 L8.2 16.4 L11.8 24 L15.2 22.5 L11.6 15 L18.6 15 Z" fill="#fff" stroke="#111" stroke-width="1.6" stroke-linejoin="round"/></svg>';
    document.body.append(ring, cursor);
    cursor.style.transform = `translate(${x - 3}px, ${y - 2}px)`;
  };
  const ease = (k) => (k < 0.5 ? 4 * k * k * k : 1 - (-2 * k + 2) ** 3 / 2);
  window.__demo = {
    ensure,
    move(nx, ny, ms) {
      ensure();
      const cursor = document.getElementById("demo-cursor");
      const [x0, y0, t0] = [x, y, performance.now()];
      return new Promise((done) => {
        const step = (now) => {
          const k = Math.min(1, (now - t0) / ms);
          x = x0 + (nx - x0) * ease(k);
          y = y0 + (ny - y0) * ease(k);
          cursor.style.transform = `translate(${x - 3}px, ${y - 2}px)`;
          if (k < 1) requestAnimationFrame(step);
          else done();
        };
        requestAnimationFrame(step);
      });
    },
    pulse() {
      const at = `translate(${x}px, ${y}px)`;
      document.getElementById("demo-ring").animate(
        [
          { opacity: 0.95, transform: `${at} scale(.25)` },
          { opacity: 0, transform: `${at} scale(1.35)` },
        ],
        { duration: 480, easing: "ease-out" },
      );
      document
        .getElementById("demo-cursor")
        .animate(
          [
            { transform: `translate(${x - 3}px, ${y - 2}px) scale(1)` },
            { transform: `translate(${x - 3}px, ${y - 2}px) scale(.82)` },
            { transform: `translate(${x - 3}px, ${y - 2}px) scale(1)` },
          ],
          { duration: 220 },
        );
    },
    // rect: layout px at zoom 1 (layoutRect), or null to zoom out.
    zoom(rect, scale) {
      const [W, H] = [innerWidth, innerHeight];
      if (!rect) [s, tx, ty] = [1, 0, 0];
      else {
        s = scale;
        const cx = rect.x + rect.width / 2;
        const cy = rect.y + rect.height / 2;
        tx = Math.min(0, Math.max(W - s * W, W / 2 - s * cx));
        ty = Math.min(0, Math.max(H - s * H, H / 2 - s * cy));
      }
      document.getElementById("app").style.transform =
        s === 1 ? "" : `translate(${tx}px, ${ty}px) scale(${s})`;
    },
    layoutRect(el) {
      const r = el.getBoundingClientRect();
      return { x: (r.x - tx) / s, y: (r.y - ty) / s, width: r.width / s, height: r.height / s };
    },
  };
  document.addEventListener("DOMContentLoaded", ensure);
}

const frames = [];
const waits = [];
const now = () => Date.now() / 1000;

/** Output seconds for a wait of `len` real seconds: short waits shrink, long ones cap at 1.5 s. */
const waitOutput = (len) => Math.min(len, 0.4 + 0.1 * len, 1.5);

function writeConcat(end) {
  const sped = (t0, t1) => {
    let out = t1 - t0;
    for (const [a, b] of waits) {
      const overlap = Math.max(0, Math.min(t1, b) - Math.max(t0, a));
      if (overlap > 0) out -= overlap * (1 - waitOutput(b - a) / (b - a));
    }
    return Math.max(out, 0.001);
  };
  const lines = ["ffconcat version 1.0"];
  frames.forEach(([file, t], i) => {
    lines.push(`file '${file}'`, `duration ${sped(t, frames[i + 1]?.[1] ?? end).toFixed(4)}`);
  });
  lines.push(`file '${frames.at(-1)[0]}'`);
  writeFileSync(join(FRAMES, "frames.txt"), `${lines.join("\n")}\n`);
}

async function main() {
  await setup();
  // The UI runs on a spare port, which the daemon's CORS list does not allow.
  const browser = await chromium.launch({
    executablePath: chromiumPath(),
    args: ["--disable-web-security"],
  });
  const context = await browser.newContext({
    viewport: { width: 1280, height: 800 },
    deviceScaleFactor: 1.5,
    colorScheme: "light",
  });
  await context.addInitScript(
    ([api, token]) => {
      localStorage.setItem("ferrobot.daemon", api);
      window.__FERROBOT_TOKEN__ = token;
    },
    [API, TOKEN],
  );
  await context.addInitScript(overlay);
  const page = await context.newPage();
  await page.goto(UI);

  const pause = (ms) => page.waitForTimeout(ms);
  const glide = async (locator, ms = 750) => {
    const b = await locator.boundingBox();
    await page.evaluate(
      ([cx, cy, d]) => window.__demo.move(cx, cy, d),
      [b.x + b.width / 2, b.y + b.height / 2, ms],
    );
  };
  const click = async (locator) => {
    await locator.scrollIntoViewIfNeeded();
    await glide(locator);
    await page.evaluate(() => window.__demo.pulse());
    await locator.click();
    await pause(300);
  };
  const zoom = async (locator, scale) => {
    const rect = locator ? await locator.evaluate((el) => window.__demo.layoutRect(el)) : null;
    await page.evaluate(([r, sc]) => window.__demo.zoom(r, sc), [rect, scale]);
    await pause(1000);
  };
  const waitFor = async (locator) => {
    const start = now();
    await locator.waitFor({ timeout: WAIT_MS });
    waits.push([start, now()]);
  };
  const open = async (name) => {
    await click(page.locator(`button[title^="${name} ·"]`).first());
    await page.locator(`.chat[aria-label="Chat with ${name}"]`).waitFor();
  };
  // A done tool call shows the work started; then wait for the turn to end over the API.
  const finished = async (name) => {
    const start = now();
    for (let i = 0; i < WAIT_MS / 500; i++) {
      const agent = (await api("GET", "/v1/agents")).find((a) => a.name === name);
      if (agent && agent.presence !== "working" && agent.presence !== "starting") break;
      await sleep(500);
    }
    waits.push([start, now()]);
  };
  const lastTool = (name) =>
    page.locator(".tool-card.t-ok", { has: page.locator(".tool-name", { hasText: name }) }).last();

  await page.locator('.chat[aria-label="Chat with atlas"]').waitFor({ timeout: 30_000 });
  await page.evaluate(() => window.__demo.ensure());
  if (await page.locator("aside.info").isVisible()) await page.getByLabel("Hide details").click();

  const cdp = await context.newCDPSession(page);
  cdp.on("Page.screencastFrame", ({ data, metadata, sessionId }) => {
    void cdp.send("Page.screencastFrameAck", { sessionId }).catch(() => undefined);
    const last = frames.at(-1);
    if (last && metadata.timestamp - last[1] < 1 / 30) return;
    const file = `${String(frames.length).padStart(6, "0")}.jpg`;
    writeFileSync(join(FRAMES, file), Buffer.from(data, "base64"));
    frames.push([file, metadata.timestamp]);
  });
  await cdp.send("Page.startScreencast", { format: "jpeg", quality: 90, maxWidth: 1920 });

  try {
    // 1. The operator asks the lead.
    await pause(900);
    const composer = page.locator(".chat textarea");
    await zoom(composer, 1.7);
    await click(composer);
    await composer.pressSequentially(PROMPT, { delay: 40 });
    await pause(400);
    await click(page.getByLabel("Send"));
    await zoom(null);

    // 2. The lead answers and asks to create its teammates; approve the two it was told to make.
    const chat = page.locator(".chat");
    await waitFor(chat.locator(".bubble-agent").first());
    await zoom(chat.locator(".bubble-agent").first(), 1.5);
    let approved = 0;
    for (let cards = 0; approved < TEAM.length && cards < 4; cards++) {
      const card = chat.locator("form.qcard").first();
      await waitFor(card);
      await zoom(card, 1.6);
      await pause(1300);
      const text = await card.innerText();
      const ok = TEAM.some((n) => text.includes(`Create the agent "${n}"`));
      await click(card.getByRole("radio", { name: ok ? "Approve" : "Decline" }));
      await click(card.getByRole("button", { name: "Send answers" }));
      if (ok) approved += 1;
      await pause(500);
    }
    await zoom(null);
    await pause(800);
    await zoom(page.locator('nav[aria-label="Agents"]'), 1.6);
    await pause(1500);
    await zoom(null);

    // 3. The teammates work on their own computers.
    await open("scout");
    await waitFor(lastTool("bash"));
    await finished("scout");
    const scoutTool = lastTool("bash");
    await click(scoutTool.locator(".tool-head"));
    await zoom(scoutTool, 1.45);
    await pause(2200);
    await zoom(null);
    if (!(await page.locator("aside.info").isVisible()))
      await click(page.getByLabel("Show details"));
    await click(page.getByRole("tab", { name: "Computer" }));
    await zoom(page.locator("aside.info .computer"), 1.5);
    await pause(1600);
    await zoom(null);

    await open("quill");
    await waitFor(lastTool("write_file"));
    await finished("quill");
    const quillTool = lastTool("write_file");
    await click(quillTool.locator(".tool-head"));
    await zoom(quillTool, 1.35);
    await pause(2500);
    await zoom(null);

    // 4. Reports come back; the lead shares the post and the operator opens it.
    await open("atlas");
    const file = page.locator(".chat button.file-card").last();
    await waitFor(file);
    await pause(400);
    await zoom(page.locator(".chat .bubble-agent").last(), 1.5);
    await pause(2200);
    await zoom(null);
    await click(file);
    const viewer = page.locator(".dialog");
    await viewer.waitFor();
    await pause(400);
    await zoom(viewer, 1.3);
    await pause(3500);
    await zoom(null);
    await pause(1200);
  } finally {
    await cdp.send("Page.stopScreencast").catch(() => undefined);
    await page.screenshot({ path: join(FRAMES, "last.png") }).catch(() => undefined);
    if (frames.length) writeConcat(now());
    await browser.close();
  }
}

await main();
