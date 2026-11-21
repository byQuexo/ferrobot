import type { Connector } from "../api.ts";

/** A connector row's status, from its last connection check. */
export function checkLabel(c: Connector, checking: boolean): string {
  if (checking) return "checking…";
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
  return { ok: "p-awake", needs_sign_in: "p-starting", failed: "p-error" }[c.check.status];
}
