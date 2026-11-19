// Minimal chat UI. Renders with textContent only (no innerHTML), so agent
// output can never inject markup.
import { DaemonClient, type Agent, type StoredEvent, type UiEvent } from "./api.js";

const client = new DaemonClient(localStorage.getItem("ferrobot.daemon") ?? undefined);

const $ = <T extends HTMLElement>(id: string): T => {
  const el = document.getElementById(id);
  if (!el) throw new Error(`missing #${id}`);
  return el as T;
};

const ui = {
  agents: $<HTMLUListElement>("agents"),
  timeline: $<HTMLOListElement>("timeline"),
  title: $<HTMLHeadingElement>("title"),
  health: $<HTMLSpanElement>("health"),
  error: $<HTMLParagraphElement>("error"),
  newAgent: $<HTMLFormElement>("new-agent"),
  composer: $<HTMLFormElement>("composer"),
  interrupt: $<HTMLButtonElement>("interrupt"),
  stop: $<HTMLButtonElement>("stop"),
  wake: $<HTMLButtonElement>("wake"),
};

const state = {
  agents: new Map<string, Agent>(),
  selected: null as string | null,
  lastEventId: 0,
};

function showError(err: unknown): void {
  ui.error.textContent = err instanceof Error ? err.message : String(err);
}

function renderAgents(): void {
  ui.agents.replaceChildren(
    ...[...state.agents.values()]
      .sort((a, b) => a.name.localeCompare(b.name))
      .map((agent) => {
        const li = document.createElement("li");
        li.className = agent.id === state.selected ? "selected" : "";
        const dot = document.createElement("span");
        dot.className = `dot ${agent.connected ? "connected" : agent.status === "failed" ? "failed" : ""}`;
        const name = document.createElement("span");
        name.append(dot, agent.name);
        const status = document.createElement("span");
        status.className = "muted";
        status.textContent = agent.status;
        li.append(name, status);
        li.onclick = () => { void select(agent.id); };
        return li;
      }),
  );
}

function describe(event: StoredEvent): { cls: string; text: string } | null {
  const p = event.payload;
  const str = (k: string): string => (typeof p[k] === "string" ? p[k] : "");
  switch (event.kind) {
    case "message_in":
      return { cls: "in", text: `${str("from")}: ${str("body")}` };
    case "assistant_text":
      return { cls: "out", text: str("text") };
    case "tool_use":
      return { cls: "tool", text: `⚙ ${str("name")} ${JSON.stringify(p["input"])}` };
    case "tool_denied":
      return { cls: "denied", text: `⛔ ${str("name")}: ${str("reason")}` };
    case "turn_done":
      return str("outcome") === "completed"
        ? null
        : { cls: str("outcome") === "failed" ? "failed" : "meta", text: `turn ${str("outcome")} ${str("error")}`.trim() };
    case "runner_connected":
      return { cls: "meta", text: `computer online (${str("driver")})` };
    case "runner_disconnected":
      return { cls: "meta", text: "computer offline" };
    default:
      return null;
  }
}

function appendEvents(events: StoredEvent[]): void {
  for (const event of events) {
    state.lastEventId = Math.max(state.lastEventId, event.id);
    const view = describe(event);
    if (!view) continue;
    const li = document.createElement("li");
    li.className = view.cls;
    li.textContent = view.text;
    ui.timeline.append(li);
  }
  ui.timeline.scrollTop = ui.timeline.scrollHeight;
}

function setControls(): void {
  const has = state.selected !== null;
  for (const el of [ui.interrupt, ui.stop, ui.wake]) el.disabled = !has;
  for (const el of ui.composer.querySelectorAll<HTMLTextAreaElement | HTMLButtonElement>("textarea, button")) {
    el.disabled = !has;
  }
}

async function select(id: string): Promise<void> {
  state.selected = id;
  state.lastEventId = 0;
  ui.title.textContent = state.agents.get(id)?.name ?? id;
  ui.timeline.replaceChildren();
  renderAgents();
  setControls();
  appendEvents(await client.events(id));
}

function onUiEvent(e: UiEvent): void {
  switch (e.type) {
    case "agent":
      state.agents.set(e.agent.id, { ...e.agent, connected: e.connected });
      renderAgents();
      break;
    case "agent_deleted":
      state.agents.delete(e.agent_id);
      if (state.selected === e.agent_id) state.selected = null;
      renderAgents();
      setControls();
      break;
    case "event":
      if (e.event.agent_id === state.selected && e.event.id > state.lastEventId) appendEvents([e.event]);
      break;
    case "message":
      break;
  }
}

async function refresh(): Promise<void> {
  const agents = await client.listAgents();
  state.agents = new Map(agents.map((a) => [a.id, a]));
  renderAgents();
  if (state.selected) appendEvents(await client.events(state.selected, state.lastEventId));
}

ui.newAgent.addEventListener("submit", (ev) => {
  ev.preventDefault();
  const form = new FormData(ui.newAgent);
  const name = String(form.get("name") ?? "").trim();
  const instructions = String(form.get("instructions") ?? "");
  ui.error.textContent = "";
  client
    .createAgent(name, instructions)
    .then(async (agent) => {
      ui.newAgent.reset();
      state.agents.set(agent.id, agent);
      await select(agent.id);
    })
    .catch(showError);
});

ui.composer.addEventListener("submit", (ev) => {
  ev.preventDefault();
  const area = ui.composer.querySelector("textarea");
  const body = area?.value.trim();
  if (!state.selected || !area || !body) return;
  client
    .send(state.selected, body)
    .then(() => { area.value = ""; })
    .catch(showError);
});

ui.composer.querySelector("textarea")?.addEventListener("keydown", (ev) => {
  if (ev.key === "Enter" && !ev.shiftKey) {
    ev.preventDefault();
    ui.composer.requestSubmit();
  }
});

for (const action of ["interrupt", "stop", "wake"] as const) {
  ui[action].addEventListener("click", () => {
    if (state.selected) client.action(state.selected, action).catch(showError);
  });
}

client
  .health()
  .then((h) => { ui.health.textContent = `v${h.version} · ${h.backend} · ${h.driver}`; })
  .catch(() => { showError(`daemon not reachable at ${client.base}`); });
client.subscribe(onUiEvent, () => { void refresh().catch(showError); });
refresh().catch(showError);
setControls();
