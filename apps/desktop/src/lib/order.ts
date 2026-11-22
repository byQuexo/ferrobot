// The operator's order of the agent list, kept on this computer only.

const KEY = "ferrobot.agent-order";

interface Orderable {
  id: string;
  name: string;
  pinned?: boolean | null;
}

/** Pinned agents first; each group in the saved order, agents not in it after those, by name. */
export function orderAgents<T extends Orderable>(agents: T[], saved: string[]): T[] {
  const rank = new Map(saved.map((id, i) => [id, i]));
  const pos = (a: T): number => rank.get(a.id) ?? Number.MAX_SAFE_INTEGER;
  return [...agents].sort(
    (a, b) =>
      Number(Boolean(b.pinned)) - Number(Boolean(a.pinned)) ||
      pos(a) - pos(b) ||
      a.name.localeCompare(b.name),
  );
}

/** `order` with `id` moved next to `target` (before it, or after it with `after`). */
export function moveId(order: string[], id: string, target: string, after: boolean): string[] {
  if (id === target) return order;
  const rest = order.filter((x) => x !== id);
  const at = rest.indexOf(target);
  if (at < 0) return order;
  rest.splice(after ? at + 1 : at, 0, id);
  return rest;
}

export function loadOrder(): string[] {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(raw) ? raw.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function saveOrder(order: string[]): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(order));
  } catch {
    // Storage can be unavailable; the order then lasts until the app closes.
  }
}

/**
 * The agent to show: the remembered one while it exists, else the first pinned agent, then by
 * name. `undefined` keeps the current choice: before the list loaded, an empty map is no answer.
 */
export function pickSelection<T extends Orderable>(
  agents: ReadonlyMap<string, T>,
  selected: string | null,
  loaded: boolean,
): string | null | undefined {
  if (!loaded || (selected && agents.has(selected))) return undefined;
  const first = [...agents.values()].sort(
    (a, b) => Number(Boolean(b.pinned)) - Number(Boolean(a.pinned)) || a.name.localeCompare(b.name),
  )[0];
  return first?.id ?? null;
}
