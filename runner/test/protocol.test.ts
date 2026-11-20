import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";

import { parseDaemonFrame } from "../src/protocol.ts";

const fixtures = join(import.meta.dirname, "..", "..", "protocol", "fixtures");

function load(dir: string): [string, string][] {
  return readdirSync(join(fixtures, dir))
    .filter((f) => f.endsWith(".json"))
    .map((f) => [f, readFileSync(join(fixtures, dir, f), "utf8")]);
}

describe("protocol fixtures shared with the Rust daemon", () => {
  it("parses every daemon frame", () => {
    const frames = load("daemon");
    assert.ok(frames.length >= 4);
    for (const [name, text] of frames) {
      assert.doesNotThrow(() => parseDaemonFrame(text), name);
    }
  });

  it("runner fixtures have the shapes the runner emits", () => {
    const types = new Set(
      load("runner").map(([, text]) => (JSON.parse(text) as { type: string }).type),
    );
    assert.deepEqual([...types].sort(), ["event", "hello", "turn_done"]);
  });

  it("rejects malformed frames", () => {
    assert.throws(() => parseDaemonFrame(`{"type":"turn","turn_id":"t"}`));
    assert.throws(() => parseDaemonFrame(`{"type":"nope"}`));
    assert.throws(() => parseDaemonFrame("not json"));
  });
});
