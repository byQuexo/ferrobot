// Typed client for the ferrobotd UI API (v1).

import type { components } from "./generated/api-types.js";

export const DEFAULT_DAEMON = "http://127.0.0.1:7878";

// Wire types are generated from protocol/openapi.json (`npm run gen:api`),
// which in turn is generated from the daemon's Rust types. Do not hand-edit.
type Schemas = components["schemas"];
export type Agent = Schemas["AgentView"];
export type StoredEvent = Schemas["StoredEvent"];
export type MailMessage = Schemas["MailMessage"];
export type UiEvent = Schemas["UiEvent"];
export type Health = Schemas["Health"];
export type Interrupted = Schemas["Interrupted"];
type CreateAgent = Schemas["CreateAgent"];
type PostMessage = Schemas["PostMessage"];

export class ApiError extends Error {
  override name = "ApiError";
}

export class DaemonClient {
  readonly base: string;

  constructor(base: string = DEFAULT_DAEMON) {
    this.base = base.replace(/\/+$/, "");
  }

  async #request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const init: RequestInit = { method };
    if (body !== undefined) {
      init.headers = { "Content-Type": "application/json" };
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
      throw new ApiError(message);
    }
    return (res.status === 204 ? undefined : await res.json()) as T;
  }

  health(): Promise<Health> {
    return this.#request("GET", "/v1/health");
  }

  listAgents(): Promise<Agent[]> {
    return this.#request("GET", "/v1/agents");
  }

  createAgent(name: string, instructions: string): Promise<Agent> {
    const req: CreateAgent = { name, instructions };
    return this.#request("POST", "/v1/agents", req);
  }

  events(agentId: string, after = 0): Promise<StoredEvent[]> {
    return this.#request("GET", `/v1/agents/${agentId}/events?after=${String(after)}`);
  }

  send(agentId: string, body: string): Promise<MailMessage> {
    const req: PostMessage = { body };
    return this.#request("POST", `/v1/agents/${agentId}/messages`, req);
  }

  action(agentId: string, action: "interrupt" | "stop" | "wake"): Promise<unknown> {
    return this.#request("POST", `/v1/agents/${agentId}/${action}`);
  }

  /** Live updates; reconnects with backoff. Returns a stop function. */
  subscribe(onEvent: (e: UiEvent) => void, onReconnect: () => void): () => void {
    let ws: WebSocket | null = null;
    let stopped = false;
    let attempt = 0;
    const connect = (): void => {
      ws = new WebSocket(`${this.base.replace(/^http/, "ws")}/v1/ws`);
      ws.onopen = () => {
        if (attempt > 0) onReconnect();
        attempt = 0;
      };
      ws.onmessage = (msg) => {
        if (typeof msg.data === "string") onEvent(JSON.parse(msg.data) as UiEvent);
      };
      ws.onclose = () => {
        if (stopped) return;
        const delay = Math.min(10_000, 500 * 2 ** attempt++);
        setTimeout(connect, delay);
      };
    };
    connect();
    return () => {
      stopped = true;
      ws?.close();
    };
  }
}
