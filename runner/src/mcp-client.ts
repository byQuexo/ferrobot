// Thin wrapper around the official MCP client for the daemon's tool server.
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";

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

  /** Calls a tool and returns its structured result. Throws on tool errors. */
  async call(name: string, args: Record<string, unknown>): Promise<unknown> {
    const client = await this.#connect();
    const result = await client.callTool({ name, arguments: args });
    if (result.isError === true) {
      throw new Error(`tool ${name} failed: ${JSON.stringify(result.content)}`);
    }
    return result.structuredContent ?? result.content;
  }

  async close(): Promise<void> {
    const client = this.#client;
    this.#client = null;
    await client?.close();
  }
}
