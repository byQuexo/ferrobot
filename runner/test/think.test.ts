import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { ThinkFilter } from "../src/drivers/think.ts";

const run = (pieces: string[]): string => {
  const f = new ThinkFilter();
  return pieces.map((p) => f.push(p)).join("") + f.finish();
};

describe("think filter", () => {
  it("drops reasoning however it is chunked", () => {
    const reply = "Thinking: they asked twice. Keep it short.</think:6124c78e>Yes, the Roborock.";
    for (const size of [1, 3, 7, reply.length]) {
      const pieces = reply.match(new RegExp(`[\\s\\S]{1,${String(size)}}`, "g")) ?? [];
      assert.equal(run(pieces), "Yes, the Roborock.", `chunks of ${String(size)}`);
    }
    assert.equal(run(["<think>plan</think>\n\nDone."]), "Done.");
  });

  it("passes plain answers through at once", () => {
    const f = new ThinkFilter();
    assert.equal(f.push("Sure"), "Sure");
    assert.equal(f.push(", done."), ", done.");
    assert.equal(f.finish(), "");
    assert.equal(run(["The ", "thing is fine."]), "The thing is fine.");
    assert.equal(run(["<b>bold</b>"]), "<b>bold</b>");
  });

  it("drops an unclosed think block but keeps a bare Thinking: answer", () => {
    assert.equal(run(["<think>never closed"]), "");
    assert.equal(run(["Thinking: out loud is fine"]), "Thinking: out loud is fine");
    assert.equal(run(["Th"]), "Th");
  });
});
