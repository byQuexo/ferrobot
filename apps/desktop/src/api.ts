// Typed client for the ferrobotd UI API (v1). The desktop app is a thin
// client: every action is one of these HTTP calls, and live updates come
// from the /v1/ws stream.
import type { components } from "./generated/api-types.ts";

export const DEFAULT_DAEMON = "http://127.0.0.1:7878";

// Wire types are generated from protocol/openapi.json (`npm run gen:api`),
// which in turn is generated from the daemon's Rust types. Do not hand-edit.
type Schemas = components["schemas"];
export type Agent = Schemas["AgentView"];
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

export class ApiError extends Error {
  override name = "ApiError";
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

const enc = encodeURIComponent;

export class DaemonClient {
  readonly base: string;
  readonly #token: string;

  /** `token`: the daemon's operator token (`<data dir>/api-token`). */
  constructor(base: string = DEFAULT_DAEMON, token = "") {
    this.base = base.replace(/\/+$/, "");
    this.#token = token;
  }

  async #request<T>(method: string, path: string, body?: unknown, contentType?: string): Promise<T> {
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
    return (res.status === 204 ? undefined : await res.json()) as T;
  }

  /**
   * Absolute URL for a path returned by the API (e.g. `avatar_url`), for
   * `<img>` and WebSocket requests, which cannot send headers: the token goes
   * in the `access_token` query parameter (accepted on GET only).
   */
  url(path: string): string {
    if (!this.#token) return `${this.base}${path}`;
    const sep = path.includes("?") ? "&" : "?";
    return `${this.base}${path}${sep}access_token=${encodeURIComponent(this.#token)}`;
  }

  health = (): Promise<Health> => this.#request("GET", "/v1/health");

  // agents
  listAgents = (): Promise<Agent[]> => this.#request("GET", "/v1/agents");
  createAgent = (req: CreateAgent): Promise<Agent> => this.#request("POST", "/v1/agents", req);
  updateAgent = (id: string, req: UpdateAgent): Promise<Agent> =>
    this.#request("PATCH", `/v1/agents/${id}`, req);
  deleteAgent = (id: string): Promise<void> => this.#request("DELETE", `/v1/agents/${id}`);
  listProviders = (): Promise<Provider[]> => this.#request("GET", "/v1/providers");
  createProvider = (req: CreateProvider): Promise<Provider> => this.#request("POST", "/v1/providers", req);
  updateProvider = (id: string, req: UpdateProvider): Promise<Provider> =>
    this.#request("PATCH", `/v1/providers/${id}`, req);
  deleteProvider = (id: string): Promise<void> => this.#request("DELETE", `/v1/providers/${id}`);
  setAvatar = (id: string, image: Blob): Promise<Agent> => this.#request("PUT", `/v1/agents/${id}/avatar`, image);
  removeAvatar = (id: string): Promise<Agent> => this.#request("DELETE", `/v1/agents/${id}/avatar`);

  // chat
  messages = (id: string): Promise<MailMessage[]> => this.#request("GET", `/v1/agents/${id}/messages`);
  events = (id: string, after = 0): Promise<StoredEvent[]> =>
    this.#request("GET", `/v1/agents/${id}/events?after=${String(after)}`);
  send = (id: string, body: string): Promise<MailMessage> =>
    this.#request("POST", `/v1/agents/${id}/messages`, { body });
  markRead = (id: string): Promise<void> => this.#request("POST", `/v1/agents/${id}/read`);
  interrupt = (id: string): Promise<unknown> => this.#request("POST", `/v1/agents/${id}/interrupt`);
  answerQuestion = (id: string, question: number, answers: QuestionAnswer[]): Promise<QuestionAnswered> =>
    this.#request("POST", `/v1/agents/${id}/questions/${String(question)}/answer`, { answers });

  // memory
  memory = (id: string, q = ""): Promise<MemoryEntry[]> =>
    this.#request("GET", `/v1/agents/${id}/memory?q=${enc(q)}`);
  remember = (id: string, key: string, content: string): Promise<void> =>
    this.#request("POST", `/v1/agents/${id}/memory`, { key, content });
  forget = (id: string, key: string): Promise<void> =>
    this.#request("DELETE", `/v1/agents/${id}/memory/${enc(key)}`);

  // routines
  routines = (id: string): Promise<Routine[]> => this.#request("GET", `/v1/agents/${id}/routines`);
  createRoutine = (id: string, req: CreateRoutine): Promise<Routine> =>
    this.#request("POST", `/v1/agents/${id}/routines`, req);
  updateRoutine = (rid: number, req: UpdateRoutine): Promise<Routine> =>
    this.#request("PATCH", `/v1/routines/${String(rid)}`, req);
  deleteRoutine = (rid: number): Promise<void> => this.#request("DELETE", `/v1/routines/${String(rid)}`);
  runRoutine = (rid: number): Promise<MailMessage> => this.#request("POST", `/v1/routines/${String(rid)}/run`);
  routineRuns = (rid: number): Promise<RoutineRun[]> => this.#request("GET", `/v1/routines/${String(rid)}/runs`);

  // sandbox
  sandbox = (id: string): Promise<SandboxInfo> => this.#request("GET", `/v1/agents/${id}/sandbox`);
  sleep = (id: string): Promise<void> => this.#request("POST", `/v1/agents/${id}/stop`);
  wake = (id: string): Promise<void> => this.#request("POST", `/v1/agents/${id}/wake`);
  snapshots = (id: string): Promise<Snapshot[]> => this.#request("GET", `/v1/agents/${id}/snapshots`);
  snapshot = (id: string, label: string): Promise<Snapshot> =>
    this.#request("POST", `/v1/agents/${id}/snapshots`, { label });
  restore = (id: string, sid: number): Promise<void> =>
    this.#request("POST", `/v1/agents/${id}/snapshots/${String(sid)}/restore`);
  deleteSnapshot = (id: string, sid: number): Promise<void> =>
    this.#request("DELETE", `/v1/agents/${id}/snapshots/${String(sid)}`);

  // agent-to-agent conversations (read-only)
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
