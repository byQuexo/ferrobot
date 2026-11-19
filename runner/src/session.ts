// Persists the model session id on the sandbox disk so a restarted or
// re-woken runner resumes the same conversation.
import { mkdir, readFile, rename, writeFile, appendFile } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";

const stateSchema = z.object({
  sessionId: z.string().nullable(),
  updatedAt: z.string(),
});
export type SessionState = z.infer<typeof stateSchema>;

export class SessionStore {
  readonly #dir: string;
  readonly #file: string;

  constructor(stateDir: string) {
    this.#dir = stateDir;
    this.#file = join(stateDir, "session.json");
  }

  async load(): Promise<SessionState> {
    try {
      return stateSchema.parse(JSON.parse(await readFile(this.#file, "utf8")));
    } catch (err) {
      if (isNotFound(err)) return { sessionId: null, updatedAt: new Date(0).toISOString() };
      throw err;
    }
  }

  /** Atomic write: temp file + rename. */
  async save(sessionId: string | null): Promise<void> {
    await mkdir(this.#dir, { recursive: true, mode: 0o700 });
    const tmp = `${this.#file}.${String(process.pid)}.tmp`;
    const state: SessionState = { sessionId, updatedAt: new Date().toISOString() };
    await writeFile(tmp, JSON.stringify(state, null, 2), { mode: 0o600 });
    await rename(tmp, this.#file);
  }

  /** Appends one line to the tool audit log. */
  async audit(entry: Record<string, unknown>): Promise<void> {
    await mkdir(this.#dir, { recursive: true, mode: 0o700 });
    await appendFile(
      join(this.#dir, "audit.jsonl"),
      `${JSON.stringify({ ts: new Date().toISOString(), ...entry })}\n`,
      { mode: 0o600 },
    );
  }
}

function isNotFound(err: unknown): boolean {
  return typeof err === "object" && err !== null && "code" in err && err.code === "ENOENT";
}
