// Thin wrapper around the official MCP client for the daemon's tool server.
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import {
  StreamableHTTPClientTransport,
  StreamableHTTPError,
} from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import type { RequestOptions } from "@modelcontextprotocol/sdk/shared/protocol.js";
import type { Tool } from "@modelcontextprotocol/sdk/types.js";

export class FerrobotMcp {
  readonly #url: URL;
  readonly #token: string;
  #client: Client | null = null;

  constructor(url: URL, token: string) {
    this.#url = url;
    this.#token = token;
  }

  async #connect(): Promise<Client> {
    if (this.#client) return this.#client;
    const client = new Client({ name: "ferrobot-runner", version: "0.1.0" });
    const transport = new StreamableHTTPClientTransport(this.#url, {
      requestInit: { headers: { Authorization: `Bearer ${this.#token}` } },
    });
    await client.connect(transport);
    this.#client = client;
    return client;
  }

  async #request<T>(send: (client: Client) => Promise<T>): Promise<T> {
    try {
      return await send(await this.#connect());
    } catch (err) {
      // A restarted daemon forgets MCP sessions and answers 404; the request
      // was not executed, so open a fresh session and retry once.
      if (!isSessionLost(err)) throw err;
      await this.close().catch(() => undefined);
      return send(await this.#connect());
    }
  }

  /** Calls a tool and returns its structured result. Throws on tool errors. */
  async call(name: string, args: Record<string, unknown>, options?: RequestOptions): Promise<unknown> {
    const result = await this.#request((client) => client.callTool({ name, arguments: args }, undefined, options));
    if (result.isError === true) {
      throw new Error(`tool ${name} failed: ${JSON.stringify(result.content)}`);
    }
    return result.structuredContent ?? result.content;
  }

  async listTools(options?: RequestOptions): Promise<Tool[]> {
    return (await this.#request((client) => client.listTools(undefined, options))).tools;
  }

  async close(): Promise<void> {
    const client = this.#client;
    this.#client = null;
    await client?.close();
  }
}

export function isSessionLost(err: unknown): boolean {
  return err instanceof StreamableHTTPError && err.code === 404;
}
