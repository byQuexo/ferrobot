import type { Diagnostics } from "../api.ts";
import { duration } from "./format.ts";

const plural = (n: number, word: string): string => `${String(n)} ${word}${n === 1 ? "" : "s"}`;

/** Label and value of each daemon fact, for the Diagnostics list and the report. */
export function diagnosticsRows(app: string, d: Diagnostics): [string, string][] {
  const imageId = d.runner_image_id?.replace(/^sha256:/, "").slice(0, 12);
  return [
    ["App", app],
    ["Daemon", `${d.version} (${d.build})`],
    ["System", `${d.os} ${d.arch}`],
    ["Sandbox", `${d.backend}, ${d.sandbox_ready ? "ready" : (d.sandbox_error ?? "not ready")}`],
    [
      "Runner image",
      d.runner_image ? `${d.runner_image}${imageId ? ` (${imageId})` : ""}` : "none",
    ],
    ["Driver", d.driver],
    ["Anthropic key", d.anthropic_key ? "set" : "not set"],
    [
      "Setup",
      `${plural(d.agents, "agent")}, ${plural(d.providers, "provider")}, ${plural(d.connectors, "connector")}`,
    ],
    ["Storage", d.storage],
    ["Uptime", duration(d.uptime_s)],
  ];
}

/** Plain text for a bug report: versions, status, counts, data folder and the log. */
export function diagnosticsReport(app: string, d: Diagnostics, log: string[]): string {
  return [
    "ferrobot diagnostics",
    ...diagnosticsRows(app, d).map(([label, value]) => `${label}: ${value}`),
    `Data folder: ${d.data_dir}`,
    "",
    `Daemon log (last ${plural(log.length, "line")}):`,
    ...log,
  ].join("\n");
}
