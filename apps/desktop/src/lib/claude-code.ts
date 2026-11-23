import type { ClaudeCodeStatus } from "../api.ts";

export const CLAUDE_CODE_NOTICE =
  "Uses your own Claude subscription seat. All agents share your seat’s limits. Do not share your seat.";

/** Whether Claude Code agents can run, and what to do if not, in plain words. */
export function claudeCodeState(s: ClaudeCodeStatus | null): { ready: boolean; text: string } {
  if (!s) return { ready: false, text: "Checking Claude Code…" };
  if (!s.installed)
    return { ready: false, text: "Claude Code is not installed on the daemon’s computer." };
  if (!s.logged_in)
    return {
      ready: false,
      text: "Claude Code is not logged in. Run claude auth login in a terminal on the daemon’s computer, then check again.",
    };
  const how = s.auth_method ? ` with ${s.auth_method}` : "";
  return { ready: true, text: `${s.version ?? "Claude Code"} · logged in${how}` };
}
