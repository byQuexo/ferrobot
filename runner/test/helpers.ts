import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import type { TurnContext } from "../src/drivers/types.ts";
import type { TurnEvent } from "../src/protocol.ts";

/** A turn of user message #7 "hi" to alice; its events collect in `events`. */
export function context(overrides: Partial<TurnContext> = {}): {
  ctx: TurnContext;
  events: TurnEvent[];
} {
  const events: TurnEvent[] = [];
  const ctx: TurnContext = {
    turn: {
      type: "turn",
      turn_id: "t1",
      message_id: 7,
      sender: { kind: "user" },
      sender_name: "user",
      body: "hi",
    },
    instructions: "Be brief.",
    agentName: "alice",
    sessionId: null,
    signal: new AbortController().signal,
    emit: (e) => events.push(e),
    ...overrides,
  };
  return { ctx, events };
}

/** A `name` binary that runs a fake from fixtures/ through a shell wrapper, like a real one on PATH. */
export function fakeBin(name: string, fixture: string): string {
  const bin = join(mkdtempSync(join(tmpdir(), `ferrobot-${name}-bin-`)), name);
  writeFileSync(
    bin,
    `#!/bin/sh\nexec "${process.execPath}" --no-warnings "${join(import.meta.dirname, "fixtures", fixture)}" "$@"\n`,
    { mode: 0o755 },
  );
  return bin;
}

/** The messages a fake recorded in `dir`. */
export function fakeLog<R extends { id?: string | number; method?: string }>(dir: string) {
  const received = (): R[] =>
    readFileSync(join(dir, "received.jsonl"), "utf8")
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line) as R);
  return {
    received,
    sent: (method: string): R[] => received().filter((m) => m.method === method),
    answer: (id: string): R | undefined => received().find((m) => m.id === id && !m.method),
  };
}
