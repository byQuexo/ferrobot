import type { ExecOutput } from "../api.ts";

/** The agent's home in its computer (the runner image's user). */
export const HOME = "/home/node";
export const CONSOLE_TIMEOUT_S = 60;
/** Bytes of a file the browser shows. */
export const VIEW_BYTES = 256 * 1024;
const PWD_MARK = "\u001e";

export interface DirEntry {
  name: string;
  dir: boolean;
  size: number;
}

/** Lists `dir` as `type\tsize\tname\0` per entry, symlinks followed (GNU find). */
export const listCommand = (dir: string): string[] => [
  "find",
  dir,
  "-mindepth",
  "1",
  "-maxdepth",
  "1",
  "-printf",
  "%Y\\t%s\\t%f\\0",
];

export function parseListing(stdout: string): DirEntry[] {
  const entries: DirEntry[] = [];
  for (const record of stdout.split("\0")) {
    const [type, size, ...name] = record.split("\t");
    if (!type || name.length === 0) continue;
    entries.push({ name: name.join("\t"), dir: type === "d", size: Number(size) || 0 });
  }
  return entries.sort((a, b) => Number(b.dir) - Number(a.dir) || a.name.localeCompare(b.name));
}

export const readCommand = (path: string): string[] => ["head", "-c", String(VIEW_BYTES), path];

export const joinPath = (dir: string, name: string): string =>
  dir === "/" ? `/${name}` : `${dir}/${name}`;

export const parentOf = (dir: string): string => dir.slice(0, dir.lastIndexOf("/")) || "/";

export const shortPath = (path: string): string =>
  path === HOME || path.startsWith(`${HOME}/`) ? `~${path.slice(HOME.length)}` : path;

/** Runs `line` in bash with a time limit and reports the directory it ended in, so `cd` sticks. */
export const consoleCommand = (line: string): string[] => [
  "timeout",
  String(CONSOLE_TIMEOUT_S),
  "bash",
  "-c",
  `${line}\n__s=$?; printf '\\036%s' "$PWD"; exit $__s`,
];

/** The command's own output, and its last directory (null if it never got there). */
export function parseConsole(out: ExecOutput): { stdout: string; cwd: string | null } {
  const at = out.stdout.lastIndexOf(PWD_MARK);
  if (at < 0) return { stdout: out.stdout, cwd: null };
  return { stdout: out.stdout.slice(0, at), cwd: out.stdout.slice(at + 1) || null };
}

export const timedOut = (out: ExecOutput): boolean => out.exit_code === 124;
