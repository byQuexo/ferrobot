import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { handle } from "../src/guest-agent.ts";

const b64 = (s: string | Uint8Array): string => Buffer.from(s).toString("base64");
const text = (s: unknown): string => Buffer.from(String(s), "base64").toString();

describe("guest agent", () => {
  it("runs commands with cwd and env and reports exit codes", async () => {
    const dir = await mkdtemp(join(tmpdir(), "guest-"));
    const out = await handle({
      op: "exec",
      cmd: ["sh", "-c", "pwd; echo $GREETING; echo oops >&2; exit 3"],
      cwd: dir,
      env: { GREETING: "hi" },
    });
    assert.equal(out["exit_code"], 3);
    assert.match(text(out["stdout"]), /guest-.*\nhi\n$/);
    assert.equal(text(out["stderr"]), "oops\n");
    const missing = await handle({ op: "exec", cmd: ["no-such-binary-ferrobot"] });
    assert.equal(missing["exit_code"], 127);
  });

  it("reads and writes binary files and reports missing ones", async () => {
    const dir = await mkdtemp(join(tmpdir(), "guest-"));
    const path = join(dir, "a.bin");
    assert.deepEqual(
      await handle({ op: "write", path, data: b64(new Uint8Array([0, 159, 255])) }),
      { ok: true },
    );
    assert.deepEqual([...(await readFile(path))], [0, 159, 255]);
    assert.deepEqual(await handle({ op: "read", path }), {
      data: b64(new Uint8Array([0, 159, 255])),
    });
    assert.deepEqual(await handle({ op: "read", path: join(dir, "nope") }), { error: "not_found" });
    assert.deepEqual(await handle({ op: "write", path: join(dir, "no/dir/x"), data: "" }), {
      error: "not_found",
    });
  });

  it("answers one request per process on stdio, like socat runs it", () => {
    const script = fileURLToPath(new URL("../src/guest-agent.ts", import.meta.url));
    const run = spawnSync(process.execPath, [script], {
      input: `${JSON.stringify({ op: "exec", cmd: ["echo", "via stdio"] })}\n`,
      encoding: "utf8",
    });
    assert.equal(run.status, 0, run.stderr);
    const answer = JSON.parse(run.stdout) as Record<string, unknown>;
    assert.equal(text(answer["stdout"]), "via stdio\n");
    const bad = spawnSync(process.execPath, [script], { input: "not json\n", encoding: "utf8" });
    assert.ok((JSON.parse(bad.stdout) as { error?: string }).error);
  });
});
