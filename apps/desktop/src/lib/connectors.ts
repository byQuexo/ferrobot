import type { Connector } from "../api.ts";
import { hash } from "./avatar.ts";
import { CATALOG, type CatalogEntry, isAdded } from "./catalog.ts";

/** The catalog entry for `url` whose server only works with the operator's own OAuth app. */
export function ownAppEntry(url: string): CatalogEntry | undefined {
  return CATALOG.find((e) => e.app && e.url === url.trim());
}

/** Such a server without an OAuth app: it may list its tools, but they never work. */
export const needsOwnApp = (c: Connector): boolean =>
  !c.client_id && ownAppEntry(c.url) !== undefined;

/** The catalog entry a connector was added from, matched like `isAdded`. */
export const entryOf = (c: Connector): CatalogEntry | undefined =>
  CATALOG.find((e) => isAdded(e, [c]));

/** A catalog server that only takes a token, never an OAuth sign-in. */
const tokenOnly = (c: Connector): boolean => entryOf(c)?.auth === "token";

/** A connector row's status, from its last connection check. */
export function checkLabel(c: Connector, checking: boolean): string {
  if (checking) return "checking…";
  if (needsOwnApp(c)) return "needs your OAuth app";
  const check = c.check;
  if (!check) return "not checked yet";
  switch (check.status) {
    case "ok": {
      const n = check.tools ?? 0;
      return `connected · ${n} ${n === 1 ? "tool" : "tools"}`;
    }
    case "needs_sign_in":
      if (tokenOnly(c)) return "needs a token";
      return c.sign_in === "expired" ? "sign-in expired, sign in again" : "needs sign-in";
    case "failed":
      return `failed: ${check.error ?? "unknown error"}`;
  }
}

/** The status dot's presence class. */
export function checkDot(c: Connector, checking: boolean): string {
  if (checking || !c.check) return "p-sleeping";
  if (needsOwnApp(c)) return "p-starting";
  return { ok: "p-awake", needs_sign_in: "p-starting", failed: "p-error" }[c.check.status];
}

/** The step a connector's card leads with: its OAuth app, a token in Vault or a sign-in. */
export function needOf(c: Connector): "app" | "token" | "sign_in" | undefined {
  if (needsOwnApp(c)) return "app";
  if (c.check?.status !== "needs_sign_in") return undefined;
  return tokenOnly(c) ? "token" : "sign_in";
}

/** Up to two initials for a tile: "cloudflare-docs" is "CD", "linear" is "L". */
export function monogram(name: string): string {
  const words = name.split(/[^a-z0-9]+/i).filter(Boolean);
  return (
    words
      .slice(0, 2)
      .map((w) => w.charAt(0).toUpperCase())
      .join("") || "?"
  );
}

/** A stable hue per name, so a connector keeps its tile colour everywhere. */
export const tileHue = (name: string): number => hash(name) % 360;

/** Which agents may use each connector, from every agent's connector ids. */
export function usersOf(assignments: [string, string[]][]): Map<string, string[]> {
  const users = new Map<string, string[]>();
  for (const [agent, ids] of assignments) {
    for (const id of ids) users.set(id, [...(users.get(id) ?? []), agent]);
  }
  return users;
}

/** "Used by ada and bob", "Used by ada, bob, cy and 2 more" or "Not given to an agent yet". */
export function usedByLabel(names: string[]): string {
  if (names.length === 0) return "Not given to an agent yet";
  const shown =
    names.length > 4 ? [...names.slice(0, 3), `${String(names.length - 3)} more`] : names;
  const head = shown.slice(0, -1);
  return `Used by ${head.length > 0 ? `${head.join(", ")} and ` : ""}${shown.at(-1) ?? ""}`;
}
