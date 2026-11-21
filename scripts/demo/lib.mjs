// Recording kit for the demo scenes: daemon API, Chromium with an injected cursor, zoom, captions and
// a "2x" badge, CDP screencast frames with timed segments, and the ffmpeg encodes per cut.
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";

const { DEMO_API: API, DEMO_UI: UI } = process.env;
const token = () => readFileSync(process.env.DEMO_TOKEN_FILE ?? "", "utf8").trim();
export const WAIT_MS = Number(process.env.DEMO_WAIT_S ?? 300) * 1000;

const require = createRequire(import.meta.url);
const root = execFileSync("npm", ["root", "-g"], { encoding: "utf8" }).trim();
const { chromium } = require(join(root, "@playwright/cli/node_modules/playwright"));

export async function api(method, path, body) {
  const res = await fetch(`${API}${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${token()}`,
      ...(body ? { "content-type": "application/json" } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`${method} ${path}: ${String(res.status)} ${await res.text()}`);
  return res.status === 204 ? null : res.json();
}

// shortcut: an older cached Chromium stands in when the CLI updated Playwright; `playwright install` fixes it.
function chromiumPath() {
  const expected = chromium.executablePath();
  const m = /^(.*[\\/])chromium-\d+([\\/].*)$/.exec(expected);
  if (existsSync(expected) || !m) return undefined;
  const builds = readdirSync(m[1]).filter((d) => /^chromium-\d+$/.test(d));
  builds.sort((a, b) => Number(b.slice(9)) - Number(a.slice(9)));
  return builds[0] ? `${m[1]}${builds[0]}${m[2]}` : undefined;
}

// Runs in the page: cursor, click ring, caption and badge outside #app, and a CSS zoom on #app.
function overlay() {
  let [x, y, s, tx, ty] = [900, 700, 1, 0, 0];
  const ensure = () => {
    if (document.getElementById("demo-cursor")) return;
    const style = document.createElement("style");
    style.textContent = `
      html:has(#app), body:has(#app) { overflow: hidden !important; }
      #app { transform-origin: 0 0; transition: transform 800ms cubic-bezier(.45,.05,.2,1); }
      .toast, .toasts { display: none !important; }
      #demo-cursor, #demo-ring, #demo-cap, #demo-badge { position: fixed; pointer-events: none; z-index: 2147483647; }
      #demo-cursor { left: 0; top: 0; width: 26px; height: 26px; filter: drop-shadow(0 2px 3px rgba(0,0,0,.35)); }
      #demo-ring { left: 0; top: 0; width: 40px; height: 40px; margin: -20px 0 0 -20px; border-radius: 50%; opacity: 0;
        border: 2px solid rgba(79,70,229,.85); background: rgba(79,70,229,.18); }
      #demo-cap { left: 50%; bottom: 96px; transform: translateX(-50%); max-width: 78%; padding: 11px 20px;
        border-radius: 14px; background: rgba(17,17,27,.86); color: #fff; text-align: center;
        font: 600 19px/1.35 -apple-system, "Segoe UI", system-ui, sans-serif; opacity: 0; transition: opacity 350ms; }
      #demo-badge { top: 18px; left: 50%; transform: translateX(-50%); padding: 6px 14px; border-radius: 999px;
        background: rgba(79,70,229,.95); color: #fff; font: 700 16px/1 -apple-system, system-ui, sans-serif;
        opacity: 0; transition: opacity 250ms; }`;
    document.head.append(style);
    const make = (id, html = "") => {
      const el = document.createElement("div");
      el.id = id;
      el.innerHTML = html;
      document.body.append(el);
      return el;
    };
    make("demo-ring");
    make("demo-cap");
    make("demo-badge", "2x");
    make(
      "demo-cursor",
      '<svg viewBox="0 0 26 26" width="26" height="26"><path d="M3 2 L3 21 L8.2 16.4 L11.8 24 L15.2 22.5 L11.6 15 L18.6 15 Z" fill="#fff" stroke="#111" stroke-width="1.6" stroke-linejoin="round"/></svg>',
    ).style.transform = `translate(${x - 3}px, ${y - 2}px)`;
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
      const at = (k) => `translate(${x - 3}px, ${y - 2}px) scale(${k})`;
      document.getElementById("demo-ring").animate(
        [
          { opacity: 0.95, transform: `translate(${x}px, ${y}px) scale(.25)` },
          { opacity: 0, transform: `translate(${x}px, ${y}px) scale(1.35)` },
        ],
        { duration: 480, easing: "ease-out" },
      );
      document
        .getElementById("demo-cursor")
        .animate([{ transform: at(1) }, { transform: at(0.82) }, { transform: at(1) }], {
          duration: 220,
        });
    },
    caption(text) {
      ensure();
      const cap = document.getElementById("demo-cap");
      cap.textContent = text ?? "";
      cap.style.opacity = text ? "1" : "0";
    },
    badge(on) {
      ensure();
      document.getElementById("demo-badge").style.opacity = on ? "1" : "0";
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

const now = () => Date.now() / 1000;

/** Opens the UI and returns the page plus the beat helpers that record segments. */
export async function studio(framesDir) {
  mkdirSync(framesDir, { recursive: true });
  // A spare UI port is not on the daemon's CORS list; 1420 is.
  const browser = await chromium.launch({
    executablePath: chromiumPath(),
    args: UI?.endsWith(":1420") ? [] : ["--disable-web-security"],
  });
  const context = await browser.newContext({
    viewport: { width: 1280, height: 800 },
    deviceScaleFactor: 1.5,
    colorScheme: "light",
    acceptDownloads: true,
    // The cursor and captions also go on share-link pages, which send a sandboxing CSP.
    bypassCSP: true,
  });
  await context.addInitScript(
    ([api, token]) => {
      localStorage.setItem("ferrobot.daemon", api);
      if (!localStorage.getItem("ferrobot.info")) localStorage.setItem("ferrobot.info", "closed");
      window.__FERROBOT_TOKEN__ = token;
    },
    [API, token()],
  );
  await context.addInitScript(overlay);
  let page = await context.newPage();
  await page.goto(UI);
  await page.locator(".sidebar").waitFor({ timeout: 30_000 });
  await page.evaluate(() => window.__demo.ensure());

  const frames = [];
  const segments = [];
  let cdp = null;
  let recording = false;
  const attach = async (target) => {
    const session = await context.newCDPSession(target);
    session.on("Page.screencastFrame", ({ data, metadata, sessionId }) => {
      void session.send("Page.screencastFrameAck", { sessionId }).catch(() => undefined);
      const last = frames.at(-1);
      if (last && metadata.timestamp - last[1] < 1 / 30) return;
      const file = `${String(frames.length).padStart(6, "0")}.jpg`;
      writeFileSync(join(framesDir, file), Buffer.from(data, "base64"));
      frames.push([file, metadata.timestamp]);
    });
    return session;
  };
  const screencast = async (on) => {
    recording = on;
    await (on
      ? cdp.send("Page.startScreencast", { format: "jpeg", quality: 88, maxWidth: 1920 })
      : cdp.send("Page.stopScreencast"));
  };
  cdp = await attach(page);

  const pause = (ms) => page.waitForTimeout(ms);
  const segment = async (kind, fn, label) => {
    const a = now();
    const out = await fn();
    segments.push({ kind, a, b: now(), label });
    return out;
  };
  const badged = async (fn) => {
    await page.evaluate(() => window.__demo.badge(true));
    try {
      return await fn();
    } finally {
      await page.evaluate(() => window.__demo.badge(false)).catch(() => undefined);
    }
  };
  const glide = async (locator, ms = 600) => {
    const b = await locator.boundingBox();
    await page.evaluate(
      ([cx, cy, d]) => window.__demo.move(cx, cy, d),
      [b.x + b.width / 2, b.y + b.height / 2, ms],
    );
  };
  return {
    get page() {
      return page;
    },
    pause,
    /** A pause the shorter cuts may trim. */
    hold: (ms) => segment("hold", () => pause(ms)),
    /** Waits for the model or a sandbox: shown at 2x with the badge; the middle of long waits is cut. */
    wait: (target, timeout = WAIT_MS) =>
      segment("wait", () =>
        badged(async () => {
          if (typeof target === "function") await target();
          else await target.waitFor({ timeout });
        }),
      ),
    /** Steps shown at 2x with the badge, uncut. */
    fast: (fn) => segment("fast", () => badged(fn)),
    /** Steps that a cut can leave out by listing `label` in its `skip`. */
    optional: (label, fn) => segment("part", fn, label),
    /** Off-camera work (setup between beats): the screencast pauses and the time is dropped. */
    offscreen: (fn) =>
      segment("cut", async () => {
        await screencast(false);
        try {
          return await fn();
        } finally {
          await screencast(true);
        }
      }),
    /** Moves the recording to another tab (e.g. a share link the app opened). */
    async follow(target) {
      await screencast(false);
      page = target;
      cdp = await attach(page);
      await page.waitForLoadState();
      await page.evaluate(() => window.__demo?.ensure());
      await screencast(true);
    },
    caption: (text) => page.evaluate((t) => window.__demo.caption(t), text ?? null),
    click: async (locator) => {
      await locator.scrollIntoViewIfNeeded();
      await glide(locator);
      await page.evaluate(() => window.__demo.pulse());
      await locator.click();
      await pause(300);
    },
    /** Types like a person, with the cursor parked on the field. */
    type: async (locator, text, delay = 32) => {
      await locator.pressSequentially(text, { delay });
    },
    glide,
    zoom: async (locator, scale) => {
      const rect = locator ? await locator.evaluate((el) => window.__demo.layoutRect(el)) : null;
      await page.evaluate(([r, sc]) => window.__demo.zoom(r, sc), [rect, scale]);
      await pause(850);
    },
    start: () => screencast(true),
    async finish() {
      if (recording) await screencast(false).catch(() => undefined);
      await browser.close();
      return { frames, segments, end: now() };
    },
  };
}

/** Output seconds per real second at `t`, for a cut that keeps holds up to `holdMax` s and waits
 * up to `waitMax` output s (their head and tail, at 2x). */
function rateAt(t, segments, { holdMax, waitMax, skip = [] }) {
  const inside = segments.filter(({ a, b }) => t >= a && t < b);
  if (inside.some(({ label }) => label && skip.includes(label))) return 0;
  for (const { kind, a, b } of inside) {
    if (kind === "cut") return 0;
    if (kind === "fast") return 0.5;
    if (kind === "hold") return t - a < holdMax ? 1 : 0;
    if (kind === "part") continue;
    const keep = Math.min(b - a, 2 * waitMax) / 2;
    return t - a < keep || b - t <= keep ? 0.5 : 0;
  }
  return 1;
}

/** Writes an ffconcat list for one cut and returns its length in seconds. */
function concatList(take, cut, file) {
  const { frames, segments, end } = take;
  const step = 0.01;
  const out = (t0, t1) => {
    let sum = 0;
    for (let t = t0; t < t1; t += step) sum += Math.min(step, t1 - t) * rateAt(t, segments, cut);
    return sum;
  };
  const lines = ["ffconcat version 1.0"];
  let total = 0;
  frames.forEach(([name, t], i) => {
    const d = out(t, frames[i + 1]?.[1] ?? end);
    if (d < 0.0005) return;
    total += d;
    lines.push(`file '${name}'`, `duration ${d.toFixed(4)}`);
  });
  lines.push(lines.at(-2) ?? "");
  writeFileSync(file, `${lines.join("\n")}\n`);
  return total;
}

/** Encodes one cut of a take to MP4 (and GIF when `gif` is set). */
export function encode(take, framesDir, cut) {
  const list = join(framesDir, `${cut.name}.txt`);
  const seconds = concatList(take, cut, list);
  mkdirSync(dirname(cut.mp4), { recursive: true });
  const ffmpeg = (...args) => execFileSync("ffmpeg", ["-loglevel", "error", "-y", ...args]);
  ffmpeg(
    ...["-f", "concat", "-safe", "0", "-i", list],
    ...["-vf", `fps=30,scale=${String(cut.width)}:-2:flags=lanczos`],
    ...["-c:v", "libx264", "-preset", "slow", "-crf", String(cut.crf ?? 24), "-pix_fmt", "yuv420p"],
    ...["-movflags", "+faststart", cut.mp4],
  );
  if (cut.gif) {
    const { width, fps } = cut.gif;
    ffmpeg(
      ...["-i", cut.mp4, "-vf"],
      `fps=${String(fps)},scale=${String(width)}:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=96:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle`,
      cut.gif.file,
    );
  }
  console.log(`${cut.name}: ${seconds.toFixed(1)} s`);
}
