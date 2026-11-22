import type { Connector } from "../api.ts";
import { CATALOG, type CatalogEntry } from "./catalog.ts";

/** The catalog entry for `url` whose server only works with the operator's own OAuth app. */
export function ownAppEntry(url: string): CatalogEntry | undefined {
  return CATALOG.find((e) => e.app && e.url === url.trim());
}

/** Such a server without an OAuth app: it may list its tools, but they never work. */
export const needsOwnApp = (c: Connector): boolean =>
  !c.client_id && ownAppEntry(c.url) !== undefined;

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
