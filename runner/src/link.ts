// Outbound, self-healing WebSocket link to ferrobotd. The runner always
// dials out, so a sandbox that slept or restarted simply reconnects.
import WebSocket from "ws";

import { errorMessage, log } from "./log.ts";
import { parseDaemonFrame, type DaemonFrame, type RunnerFrame } from "./protocol.ts";

export interface LinkHandlers {
  /** Frame to send right after connecting. */
  hello: () => RunnerFrame;
  onFrame: (frame: DaemonFrame) => void;
  onDisconnect: () => void;
}

export interface BackoffOptions {
  initialMs: number;
  maxMs: number;
}

/** Exponential backoff with full jitter. */
export function backoffDelay(attempt: number, opts: BackoffOptions, random = Math.random): number {
  const cap = Math.min(opts.maxMs, opts.initialMs * 2 ** Math.min(attempt, 16));
  return Math.round(cap / 2 + random() * (cap / 2));
}

export class DaemonLink {
  readonly #url: URL;
  readonly #token: string;
  readonly #handlers: LinkHandlers;
  readonly #backoff: BackoffOptions;
  #ws: WebSocket | null = null;
  #attempt = 0;
  #closed = false;
  #timer: NodeJS.Timeout | null = null;

  constructor(url: URL, token: string, handlers: LinkHandlers, backoff: BackoffOptions = { initialMs: 500, maxMs: 30_000 }) {
    this.#url = url;
    this.#token = token;
    this.#handlers = handlers;
    this.#backoff = backoff;
  }

  start(): void {
    this.#connect();
  }

  /** Sends a frame; returns false when not connected (caller decides). */
  send(frame: RunnerFrame): boolean {
    const ws = this.#ws;
    if (ws?.readyState !== WebSocket.OPEN) return false;
    ws.send(JSON.stringify(frame));
    return true;
  }

  close(): void {
    this.#closed = true;
    if (this.#timer) clearTimeout(this.#timer);
    this.#ws?.close(1000, "runner shutting down");
  }

  #connect(): void {
    if (this.#closed) return;
    const ws = new WebSocket(this.#url, {
      headers: { Authorization: `Bearer ${this.#token}` },
      handshakeTimeout: 10_000,
    });
    this.#ws = ws;

    ws.on("open", () => {
      this.#attempt = 0;
      log.info("connected to daemon", { url: this.#url.toString() });
      ws.send(JSON.stringify(this.#handlers.hello()));
    });
    ws.on("message", (data, isBinary) => {
      if (isBinary) return;
      try {
        this.#handlers.onFrame(parseDaemonFrame(rawToString(data)));
      } catch (err) {
        log.warn("dropping malformed daemon frame", { error: errorMessage(err) });
      }
    });
    ws.on("error", (err) => {
      log.warn("daemon link error", { error: err.message });
    });
    ws.on("close", () => {
      if (this.#ws === ws) this.#ws = null;
      this.#handlers.onDisconnect();
      this.#scheduleReconnect();
    });
  }

  #scheduleReconnect(): void {
    if (this.#closed) return;
    const delay = backoffDelay(this.#attempt++, this.#backoff);
    log.info("reconnecting", { inMs: delay });
    this.#timer = setTimeout(() => { this.#connect(); }, delay);
  }
}

function rawToString(data: WebSocket.RawData): string {
  if (Array.isArray(data)) return Buffer.concat(data).toString("utf8");
  if (data instanceof ArrayBuffer) return Buffer.from(data).toString("utf8");
  return data.toString("utf8");
}
