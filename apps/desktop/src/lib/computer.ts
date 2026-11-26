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

/** A folder's entries once listed, or why it could not be. */
export type Listing = DirEntry[] | { error: string };

export type TreeRow =
  | { key: string; depth: number; path: string; entry: DirEntry; open: boolean }
  | { key: string; depth: number; note: string };

/** The rows of the tree under `root`: each expanded folder's entries right below it, indented. */
export function treeRows(
  root: string,
  listings: ReadonlyMap<string, Listing>,
  expanded: ReadonlySet<string>,
  max: number,
): TreeRow[] {
  const rows: TreeRow[] = [];
  const walk = (dir: string, depth: number): void => {
    const listing = listings.get(dir);
    const note = (text: string): void => {
      rows.push({ key: `${dir}\0${text}`, depth, note: text });
    };
    if (!listing) {
      note("Loading…");
      return;
    }
    if ("error" in listing) {
      note(listing.error);
      return;
    }
    if (listing.length === 0) {
      note("This folder is empty.");
      return;
    }
    for (const entry of listing.slice(0, max)) {
      const path = joinPath(dir, entry.name);
      const open = entry.dir && expanded.has(path);
      rows.push({ key: path, depth, path, entry, open });
      if (open) walk(path, depth + 1);
    }
    if (listing.length > max) note(`${listing.length - max} more not shown`);
  };
  walk(root, 0);
  return rows;
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
