import type { Dashboard } from "../api.ts";

type Board = Pick<Dashboard, "agent_id" | "path" | "label">;

/** A dashboard's default name: the file name without `.html`, e.g. `finance.html` → `finance`. */
export function boardLabel(fileName: string): string {
  const name = fileName.replace(/\.html?$/i, "").trim();
  return (name || "Dashboard").slice(0, 60);
}

export function isBoard(list: Board[], agent: string, path: string): boolean {
  return list.some((d) => d.agent_id === agent && d.path === path);
}

/** Adds the file as the last dashboard, or removes it when it already is one. */
export function toggleBoard(list: Board[], board: Board): Board[] {
  return isBoard(list, board.agent_id, board.path)
    ? list.filter((d) => !(d.agent_id === board.agent_id && d.path === board.path))
    : [...list, board];
}
