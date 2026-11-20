import type { UpdateInfo } from "../api.ts";

/**
 * What a poll of `GET /v1/update` means while a source update runs (`null`: the daemon did not
 * answer). A fresh `idle` comes from the new process: status lives in memory.
 */
export function pollOutcome(info: UpdateInfo | null): "wait" | "done" | "restarted" {
  if (!info || info.state === "running" || info.state === "restarting") return "wait";
  return info.state === "idle" ? "restarted" : "done";
}
