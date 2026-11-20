// Guest agent for Firecracker sandboxes: one JSON request on stdin, one JSON
// answer on stdout. Inside the microVM, socat serves it on vsock port 52
// (see scripts/firecracker/ferrobot-init); the host side is
// crates/sandbox/src/firecracker/guest.rs.
import { spawn } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";

/** Per stream; the host flags anything longer as truncated. */
const MAX_OUTPUT = 1024 * 1024 + 1;

export type GuestRequest =
  | { op: "exec"; cmd: string[]; cwd?: string | null; env?: Record<string, string> }
  | { op: "read"; path: string }
  | { op: "write"; path: string; data: string };

type Answer = Record<string, unknown>;

function errorAnswer(err: unknown): Answer {
  const code = (err as { code?: unknown }).code;
  if (code === "ENOENT") return { error: "not_found" };
  return { error: err instanceof Error ? err.message : String(err) };
}

function run(cmd: string[], cwd: string, env: Record<string, string>): Promise<Answer> {
  return new Promise((resolve) => {
    const [program, ...args] = cmd;
    if (!program) {
      resolve({ error: "empty command" });
      return;
    }
    const out: Buffer[] = [];
    const err: Buffer[] = [];
    const keep = (into: Buffer[]) => (chunk: Buffer): void => {
      const have = into.reduce((n, b) => n + b.length, 0);
      if (have < MAX_OUTPUT) into.push(chunk.subarray(0, MAX_OUTPUT - have));
    };
    const child = spawn(program, args, { cwd, env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "pipe"] });
    child.stdout.on("data", keep(out));
    child.stderr.on("data", keep(err));
    child.on("error", (e) => {
      // Same exit code a shell gives for a command it cannot run.
      resolve({ exit_code: 127, stdout: "", stderr: Buffer.from(e.message).toString("base64") });
    });
    child.on("close", (code, signal) => {
      resolve({
        exit_code: code ?? (signal ? 128 : -1),
        stdout: Buffer.concat(out).toString("base64"),
        stderr: Buffer.concat(err).toString("base64"),
      });
    });
  });
}

export async function handle(req: GuestRequest): Promise<Answer> {
  try {
    switch (req.op) {
      case "exec":
        return await run(req.cmd, req.cwd ?? homedir(), req.env ?? {});
      case "read":
        return { data: (await readFile(req.path)).toString("base64") };
      case "write":
        await writeFile(req.path, Buffer.from(req.data, "base64"));
        return { ok: true };
      default:
        return { error: "unknown op" };
    }
  } catch (err) {
    return errorAnswer(err);
  }
}

async function main(): Promise<void> {
  let input = "";
  for await (const chunk of process.stdin) {
    input += String(chunk);
    if (input.includes("\n")) break;
  }
  let answer: Answer;
  try {
    answer = await handle(JSON.parse(input.split("\n")[0] ?? "") as GuestRequest);
  } catch (err) {
    answer = errorAnswer(err);
  }
  process.stdout.write(`${JSON.stringify(answer)}\n`);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) void main();
