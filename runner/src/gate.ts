// The daemon's tool-call classifier, asked before a shell command of the model's own harness
// runs. A risky command may wait there for the operator's card; a dangerous one is refused.
import type { Decision } from "./permissions.ts";

export class ToolGate {
  readonly #url: URL;
  readonly #token: string;
  readonly #fetch: typeof fetch;

  constructor(daemonUrl: URL, token: string, fetchFn: typeof fetch = fetch) {
    this.#url = new URL("/runner/tool-check", daemonUrl);
    this.#token = token;
    this.#fetch = fetchFn;
  }

  async shell(command: string): Promise<Decision> {
    try {
      const res = await this.#fetch(this.#url, {
        method: "POST",
        headers: { Authorization: `Bearer ${this.#token}`, "Content-Type": "application/json" },
        body: JSON.stringify({ command }),
      });
      if (!res.ok)
        return { allow: false, reason: `blocked: tool check failed (${String(res.status)})` };
      const answer = (await res.json()) as { allow?: unknown; reason?: unknown };
      if (answer.allow === true) return { allow: true };
      return {
        allow: false,
        reason: typeof answer.reason === "string" ? answer.reason : "blocked",
      };
    } catch {
      // Fails closed: without the daemon nothing risky may run.
      return { allow: false, reason: "blocked: the tool check is unreachable" };
    }
  }
}
