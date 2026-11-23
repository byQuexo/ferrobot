import type { components } from "./generated/api-types.ts";

export const DEFAULT_DAEMON = "http://127.0.0.1:7878";

// Wire types are generated from protocol/openapi.json (`npm run gen:api`),
// which in turn is generated from the daemon's Rust types. Do not hand-edit.
type Schemas = components["schemas"];
export type Agent = Schemas["AgentView"];
export type AgentContext = Schemas["AgentContext"];
export type Trace = Schemas["Trace"];
export type Presence = Schemas["Presence"];
export type AvatarCharacter = Schemas["AvatarCharacter"];
export type StoredEvent = Schemas["StoredEvent"];
export type MailMessage = Schemas["MailMessage"];
export type UiEvent = Schemas["UiEvent"];
export type Health = Schemas["Health"];
export type MemoryEntry = Schemas["MemoryEntry"];
export type Routine = Schemas["RoutineView"];
export type RoutineRun = Schemas["RoutineRun"];
export type SandboxInfo = Schemas["SandboxInfo"];
export type Snapshot = Schemas["SnapshotRecord"];
export type CreateAgent = Schemas["CreateAgent"];
export type UpdateAgent = Schemas["UpdateAgent"];
export type CreateRoutine = Schemas["CreateRoutine"];
export type UpdateRoutine = Schemas["UpdateRoutine"];
export type Conversation = Schemas["Conversation"];
export type QuestionCard = Schemas["QuestionCard"];
export type QuestionAnswer = Schemas["QuestionAnswer"];
export type QuestionAnswered = Schemas["QuestionAnswered"];
export type Provider = Schemas["ProviderView"];
export type ProviderKind = Schemas["ProviderKind"];
export type CreateProvider = Schemas["CreateProvider"];
export type UpdateProvider = Schemas["UpdateProvider"];
export type Connector = Schemas["ConnectorView"];
export type ToolGroup = Schemas["ToolGroup"];
export type ConnectorCheck = Schemas["ConnectorCheck"];
export type SharedFile = Schemas["SharedFileView"];
export type PreviewLink = Schemas["PreviewLink"];
export type Upload = Schemas["Upload"];
export type CreateConnector = Schemas["CreateConnector"];
export type UpdateConnector = Schemas["UpdateConnector"];
export type DeviceCode = Schemas["DeviceCode"];
export type LoginStatus = Schemas["LoginStatus"];
export type ClaudeCodeStatus = Schemas["ClaudeCodeStatus"];
export type ModelList = Schemas["ModelList"];
export type ModelInfo = Schemas["ModelInfo"];
export type VoiceView = Schemas["VoiceView"];
export type VoiceProvider = Schemas["VoiceProvider"];
export type UpdateVoice = Schemas["UpdateVoice"];
export type VoiceCatalog = Schemas["VoiceCatalog"];
export type LiveMode = Schemas["LiveMode"];
export type UsageReport = Schemas["UsageReport"];
export type UsageGroup = Schemas["UsageGroup"];
export type UsageTotals = Schemas["UsageTotals"];
export type PriceEntry = Schemas["PriceEntry"];
export type SetPrice = Schemas["SetPrice"];
export type ProviderLimits = Schemas["ProviderLimits"];
export type LimitWindow = Schemas["LimitWindow"];
export type GeneralSettings = Schemas["GeneralSettings"];
export type UpdateInfo = Schemas["UpdateInfo"];
export type Diagnostics = Schemas["Diagnostics"];
export type DiagnosticsLog = Schemas["DiagnosticsLog"];
export type ClisView = Schemas["ClisView"];
export type CallView = Schemas["CallView"];
export type Room = Schemas["Room"];
export type RoomLine = Schemas["RoomLine"];
export type RoomTranscript = Schemas["RoomTranscript"];
export type UsageGroupBy = "day" | "agent" | "provider" | "model";

export class ApiError extends Error {
  override name = "ApiError";
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

const enc = encodeURIComponent;
const memoryPath = (agent: string | null): string =>
  agent ? `/v1/agents/${agent}/memory` : "/v1/memory";

/** The ferrobotd `/v1` client: every UI action is one of these calls; live updates come from `/v1/ws`. */
export class DaemonClient {
  readonly base: string;
  readonly #token: string;

  /** `token`: the daemon's operator token (`<data dir>/api-token`). */
  constructor(base: string = DEFAULT_DAEMON, token = "") {
    this.base = base.replace(/\/+$/, "");
    this.#token = token;
  }

  async #request<T>(
    method: string,
    path: string,
    body?: unknown,
    contentType?: string,
  ): Promise<T> {
    const res = await this.#fetch(method, path, body, contentType);
    return (res.status === 204 ? undefined : await res.json()) as T;
  }

  async #fetch(
    method: string,
    path: string,
    body?: unknown,
    contentType?: string,
  ): Promise<Response> {
    const headers: Record<string, string> = {};
    if (this.#token) headers["Authorization"] = `Bearer ${this.#token}`;
    const init: RequestInit = { method, headers };
    if (body instanceof Blob) {
      headers["Content-Type"] = contentType ?? body.type;
      init.body = body;
    } else if (body !== undefined) {
      headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    const res = await fetch(`${this.base}${path}`, init);
    if (!res.ok) {
      let message = `${String(res.status)} ${res.statusText}`;
      try {
        const err = (await res.json()) as { message?: string };
        if (err.message) message = err.message;
      } catch {
        // non-JSON error body
      }
      throw new ApiError(message, res.status);
    }
    return res;
  }

  /**
   * Absolute URL for `<img>` and WebSocket requests, which cannot send headers:
   * the token goes in the `access_token` query parameter (accepted on GET only).
   */
  url(path: string): string {
    if (!this.#token) return `${this.base}${path}`;
    const sep = path.includes("?") ? "&" : "?";
    return `${this.base}${path}${sep}access_token=${encodeURIComponent(this.#token)}`;
  }

  health = (): Promise<Health> => this.#request("GET", "/v1/health");

  listAgents = (): Promise<Agent[]> => this.#request("GET", "/v1/agents");
  createAgent = (req: CreateAgent): Promise<Agent> => this.#request("POST", "/v1/agents", req);
  updateAgent = (id: string, req: UpdateAgent): Promise<Agent> =>
    this.#request("PATCH", `/v1/agents/${id}`, req);
  deleteAgent = (id: string): Promise<void> => this.#request("DELETE", `/v1/agents/${id}`);
  listProviders = (): Promise<Provider[]> => this.#request("GET", "/v1/providers");
  createProvider = (req: CreateProvider): Promise<Provider> =>
    this.#request("POST", "/v1/providers", req);
  updateProvider = (id: string, req: UpdateProvider): Promise<Provider> =>
    this.#request("PATCH", `/v1/providers/${id}`, req);
  deleteProvider = (id: string): Promise<void> => this.#request("DELETE", `/v1/providers/${id}`);
  startChatgptLogin = (id: string): Promise<DeviceCode> =>
    this.#request("POST", `/v1/providers/${id}/chatgpt-login`);
  chatgptLoginStatus = (id: string): Promise<LoginStatus> =>
    this.#request("GET", `/v1/providers/${id}/chatgpt-login`);
  claudeCode = (): Promise<ClaudeCodeStatus> => this.#request("GET", "/v1/claude-code");
  providerModels = (id: string): Promise<ModelList> =>
    this.#request("GET", `/v1/providers/${id}/models`);
  kindModels = (kind: ProviderKind): Promise<ModelList> =>
    this.#request("GET", `/v1/provider-kinds/${kind}/models`);

  listConnectors = (): Promise<Connector[]> => this.#request("GET", "/v1/connectors");
  createConnector = (req: CreateConnector): Promise<Connector> =>
    this.#request("POST", "/v1/connectors", req);
  updateConnector = (id: string, req: UpdateConnector): Promise<Connector> =>
    this.#request("PATCH", `/v1/connectors/${id}`, req);
  deleteConnector = (id: string): Promise<void> => this.#request("DELETE", `/v1/connectors/${id}`);
  startConnectorOAuth = (id: string): Promise<{ authorization_url: string }> =>
    this.#request("POST", `/v1/connectors/${id}/oauth`);
  signOutConnector = (id: string): Promise<void> =>
    this.#request("DELETE", `/v1/connectors/${id}/oauth`);
  checkConnector = (id: string): Promise<ConnectorCheck> =>
    this.#request("POST", `/v1/connectors/${id}/check`);
  agentConnectors = (id: string): Promise<string[]> =>
    this.#request("GET", `/v1/agents/${id}/connectors`);
  setAgentConnectors = (id: string, ids: string[]): Promise<string[]> =>
    this.#request("PUT", `/v1/agents/${id}/connectors`, ids);
  setAvatar = (id: string, image: Blob): Promise<Agent> =>
    this.#request("PUT", `/v1/agents/${id}/avatar`, image);
  removeAvatar = (id: string): Promise<Agent> => this.#request("DELETE", `/v1/agents/${id}/avatar`);

  messages = (id: string): Promise<MailMessage[]> =>
    this.#request("GET", `/v1/agents/${id}/messages`);
  events = (id: string, after = 0): Promise<StoredEvent[]> =>
    this.#request("GET", `/v1/agents/${id}/events?after=${String(after)}`);
  /** Up to `pages` pages (500 each) of an agent's events, oldest first. */
  allEvents = async (id: string, pages = 10): Promise<StoredEvent[]> => {
    const all: StoredEvent[] = [];
    for (let page = 0, after = 0; page < pages; page++) {
      const batch = await this.events(id, after);
      all.push(...batch);
      if (batch.length < 500) break;
      after = batch[batch.length - 1]?.id ?? after;
    }
    return all;
  };
  /** `now`: stop a running turn that cannot take the message, so it goes next. */
  send = (id: string, body: string, now = false): Promise<MailMessage> =>
    this.#request("POST", `/v1/agents/${id}/messages`, now ? { body, now } : { body });
  upload = (id: string, file: File): Promise<Upload> =>
    this.#request(
      "POST",
      `/v1/agents/${id}/uploads?name=${encodeURIComponent(file.name)}`,
      file,
      "application/octet-stream",
    );
  markRead = (id: string): Promise<void> => this.#request("POST", `/v1/agents/${id}/read`);
  /** Reacts to the agent's message (its `assistant_text` event); the agent is told. */
  react = (agent: string, event: number, emoji: string): Promise<void> =>
    this.#request("POST", `/v1/agents/${agent}/events/${String(event)}/reaction`, { emoji });
  sharedFile = (agent: string, file: number): Promise<SharedFile> =>
    this.#request("GET", `/v1/agents/${agent}/files/${String(file)}`);
  /** A ```html block from the chat, served by the daemon as a sandboxed page. */
  preview = (html: string): Promise<PreviewLink> => this.#request("POST", "/v1/previews", { html });
  interrupt = (id: string): Promise<unknown> => this.#request("POST", `/v1/agents/${id}/interrupt`);
  retry = (id: string): Promise<Agent> => this.#request("POST", `/v1/agents/${id}/retry`);
  /** The agent's next turn starts a fresh model session; the chat stays. */
  newSession = (id: string): Promise<void> => this.#request("POST", `/v1/agents/${id}/new-session`);
  agentContext = (id: string): Promise<AgentContext> =>
    this.#request("GET", `/v1/agents/${id}/context`);
  /** Shrinks the agent's context to a summary (Claude Code agents). */
  compact = (id: string): Promise<void> => this.#request("POST", `/v1/agents/${id}/compact`);
  /** One of the agent's turns as a timeline. */
  trace = (id: string, turn: string): Promise<Trace> =>
    this.#request("GET", `/v1/agents/${id}/turns/${encodeURIComponent(turn)}/trace`);
  answerQuestion = (
    id: string,
    question: number,
    answers: QuestionAnswer[],
  ): Promise<QuestionAnswered> =>
    this.#request("POST", `/v1/agents/${id}/questions/${String(question)}/answer`, { answers });

  // In the memory calls, `id` null is the global memory all agents share.
  memory = (id: string | null, q = ""): Promise<MemoryEntry[]> =>
    this.#request("GET", `${memoryPath(id)}?q=${enc(q)}`);
  remember = (id: string | null, key: string, content: string): Promise<void> =>
    this.#request("POST", memoryPath(id), { key, content });
  forget = (id: string | null, key: string): Promise<void> =>
    this.#request("DELETE", `${memoryPath(id)}/${enc(key)}`);

  routines = (id: string): Promise<Routine[]> => this.#request("GET", `/v1/agents/${id}/routines`);
  createRoutine = (id: string, req: CreateRoutine): Promise<Routine> =>
    this.#request("POST", `/v1/agents/${id}/routines`, req);
  updateRoutine = (rid: number, req: UpdateRoutine): Promise<Routine> =>
    this.#request("PATCH", `/v1/routines/${String(rid)}`, req);
  deleteRoutine = (rid: number): Promise<void> =>
    this.#request("DELETE", `/v1/routines/${String(rid)}`);
  runRoutine = (rid: number): Promise<MailMessage> =>
    this.#request("POST", `/v1/routines/${String(rid)}/run`);
  routineRuns = (rid: number): Promise<RoutineRun[]> =>
    this.#request("GET", `/v1/routines/${String(rid)}/runs`);

  sandbox = (id: string): Promise<SandboxInfo> => this.#request("GET", `/v1/agents/${id}/sandbox`);
  sleep = (id: string): Promise<void> => this.#request("POST", `/v1/agents/${id}/stop`);
  wake = (id: string): Promise<void> => this.#request("POST", `/v1/agents/${id}/wake`);
  snapshots = (id: string): Promise<Snapshot[]> =>
    this.#request("GET", `/v1/agents/${id}/snapshots`);
  snapshot = (id: string, label: string): Promise<Snapshot> =>
    this.#request("POST", `/v1/agents/${id}/snapshots`, { label });
  restore = (id: string, sid: number): Promise<void> =>
    this.#request("POST", `/v1/agents/${id}/snapshots/${String(sid)}/restore`);
  deleteSnapshot = (id: string, sid: number): Promise<void> =>
    this.#request("DELETE", `/v1/agents/${id}/snapshots/${String(sid)}`);

  voice = (): Promise<VoiceView> => this.#request("GET", "/v1/voice");
  updateVoice = (req: UpdateVoice): Promise<VoiceView> => this.#request("PUT", "/v1/voice", req);
  /** Stores (or with `null` removes) one speech provider's key; the chosen provider stays. */
  updateVoiceKey = (provider: VoiceProvider, api_key: string | null): Promise<VoiceView> =>
    this.#request("PUT", `/v1/voice/keys/${provider}`, { api_key });
  /** A link that streams `text` spoken (usually mp3), so playback starts with the first bytes. */
  speechUrl = (text: string, agent?: string): string =>
    this.url(
      `/v1/voice/speech?${new URLSearchParams({ text, ...(agent ? { agent_id: agent } : {}) }).toString()}`,
    );
  /** `text` spoken by the configured voice, or by `agent`'s own one (usually mp3). */
  speech = async (text: string, agent?: string): Promise<Blob> =>
    (await this.#fetch("POST", "/v1/voice/speech", { text, agent_id: agent ?? null })).blob();
  transcribe = async (audio: Blob): Promise<string> =>
    (await this.#request<{ text: string }>("POST", "/v1/voice/transcription", audio)).text;
  /** The account's voices and speech models (ElevenLabs; empty otherwise). */
  voiceCatalog = (): Promise<VoiceCatalog> => this.#request("GET", "/v1/voice/catalog");
  /** A provider WebSocket URL with a single-use token for live transcription. */
  voiceLive = async (mode: LiveMode, sampleRate: number): Promise<string> =>
    (
      await this.#request<{ url: string }>("POST", "/v1/voice/live", {
        mode,
        sample_rate: sampleRate,
      })
    ).url;
  /** Starts, changes or (empty) ends the voice call; returns who is in it and its room. */
  setCall = (agents: string[]): Promise<CallView> => this.#request("PUT", "/v1/call", { agents });
  /** The operator's message to everyone in the call. */
  sayInCall = (body: string): Promise<MailMessage[]> =>
    this.#request("POST", "/v1/call/messages", { body });
  /** Rooms of calls with two or more agents, newest first. */
  rooms = (): Promise<Room[]> => this.#request("GET", "/v1/rooms");
  room = (id: number): Promise<RoomTranscript> => this.#request("GET", `/v1/rooms/${String(id)}`);
  /** Group chats (named rooms), oldest first. */
  groups = (): Promise<Room[]> => this.#request("GET", "/v1/groups");
  createGroup = (name: string, members: string[]): Promise<Room> =>
    this.#request("POST", "/v1/groups", { name, members });
  updateGroup = (id: number, change: { name?: string; members?: string[] }): Promise<Room> =>
    this.#request("PATCH", `/v1/groups/${String(id)}`, change);
  deleteGroup = (id: number): Promise<void> => this.#request("DELETE", `/v1/groups/${String(id)}`);
  /** The operator's message to a group; while the group is in the call, to the call. */
  sayInGroup = (id: number, body: string): Promise<MailMessage[]> =>
    this.#request("POST", `/v1/groups/${String(id)}/messages`, { body });
  /** Calls every member of a group; the group is the call's room. */
  callGroup = (id: number): Promise<CallView> =>
    this.#request("POST", `/v1/groups/${String(id)}/call`);

  /** `to` null means until now; `daily` adds each group's `days`. */
  usage = (
    groupBy: UsageGroupBy,
    from: string,
    to: string | null,
    daily = false,
  ): Promise<UsageReport> =>
    this.#request(
      "GET",
      `/v1/usage?group_by=${groupBy}&from=${enc(from)}${to ? `&to=${enc(to)}` : ""}${daily ? "&daily=true" : ""}`,
    );
  prices = (): Promise<PriceEntry[]> => this.#request("GET", "/v1/usage/prices");
  /** Replaces all of the operator's own prices. */
  setPrices = (prices: SetPrice[]): Promise<PriceEntry[]> =>
    this.#request("PUT", "/v1/usage/prices", prices);
  /** 5-hour and weekly plan use of Claude Code and ChatGPT-signed-in Codex providers. */
  limits = (): Promise<ProviderLimits[]> => this.#request("GET", "/v1/limits");
  clis = (): Promise<ClisView> => this.#request("GET", "/v1/clis");
  /** Saves the packages and starts building their image. */
  updateClis = (packages: string[]): Promise<ClisView> =>
    this.#request("PUT", "/v1/clis", { packages });
  settings = (): Promise<GeneralSettings> => this.#request("GET", "/v1/settings");
  updateSettings = (s: GeneralSettings): Promise<GeneralSettings> =>
    this.#request("PUT", "/v1/settings", s);
  updateInfo = (): Promise<UpdateInfo> => this.#request("GET", "/v1/update");
  diagnostics = (): Promise<Diagnostics> => this.#request("GET", "/v1/diagnostics");
  diagnosticsLog = (lines: number): Promise<DiagnosticsLog> =>
    this.#request("GET", `/v1/diagnostics/log?lines=${String(lines)}`);
  /** Pulls, rebuilds and restarts a source build; poll `updateInfo` for progress. */
  startUpdate = (): Promise<UpdateInfo> => this.#request("POST", "/v1/update");

  listConversations = (): Promise<Conversation[]> => this.#request("GET", "/v1/conversations");
  conversation = (a: string, b: string): Promise<MailMessage[]> =>
    this.#request("GET", `/v1/conversations/${a}/${b}`);

  /** Live updates; reconnects with backoff. Returns a stop function. */
  subscribe(handlers: {
    onEvent: (e: UiEvent) => void;
    onStatus: (online: boolean) => void;
  }): () => void {
    let ws: WebSocket | null = null;
    let stopped = false;
    let attempt = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const connect = (): void => {
      ws = new WebSocket(this.url("/v1/ws").replace(/^http/, "ws"));
      ws.onopen = () => {
        attempt = 0;
        handlers.onStatus(true);
      };
      ws.onmessage = (msg) => {
        if (typeof msg.data === "string") handlers.onEvent(JSON.parse(msg.data) as UiEvent);
      };
      ws.onclose = () => {
        if (stopped) return;
        handlers.onStatus(false);
        const delay = Math.min(10_000, 500 * 2 ** attempt++);
        timer = setTimeout(connect, delay);
      };
    };
    connect();
    return () => {
      stopped = true;
      clearTimeout(timer);
      ws?.close();
    };
  }
}
