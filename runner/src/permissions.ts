// Tool permission policy. The sandbox (container/microVM) is the primary
// security boundary; this policy adds guard rails inside it: obviously
// destructive shell commands are refused and file tools stay inside $HOME.
import { isAbsolute, relative, resolve } from "node:path";

export type Decision = { allow: true } | { allow: false; reason: string };

const DESTRUCTIVE_SHELL: readonly { pattern: RegExp; reason: string }[] = [
  { pattern: /\brm\s+(-[a-z]*r[a-z]*f|-[a-z]*f[a-z]*r)[a-z]*\s+(\/|~|\$HOME)(\s|$)/i, reason: "recursive delete of / or $HOME" },
  { pattern: /\bmkfs(\.\w+)?\b/, reason: "formatting a filesystem" },
  { pattern: /:\(\)\s*\{\s*:\|:&\s*\};:/, reason: "fork bomb" },
  { pattern: /\b(curl|wget)\b[^|]*\|\s*(sudo\s+)?(ba|z)?sh\b/, reason: "piping a download into a shell" },
  { pattern: /\bdd\s+[^\n]*of=\/dev\//, reason: "writing to a raw device" },
];

const FILE_TOOLS = new Set(["Read", "Write", "Edit", "MultiEdit", "NotebookEdit"]);

function isInside(root: string, target: string): boolean {
  const rel = relative(resolve(root), resolve(root, target));
  return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
}

function stringField(input: Record<string, unknown>, key: string): string | undefined {
  const value = input[key];
  return typeof value === "string" ? value : undefined;
}

/** Decides whether a tool call may run. Pure, so it is easy to test. */
export function decide(toolName: string, input: Record<string, unknown>, home: string): Decision {
  if (toolName.startsWith("mcp__ferrobot__")) return { allow: true };

  if (toolName === "Bash") {
    const command = stringField(input, "command") ?? "";
    for (const rule of DESTRUCTIVE_SHELL) {
      if (rule.pattern.test(command)) return { allow: false, reason: `blocked: ${rule.reason}` };
    }
    return { allow: true };
  }

  if (FILE_TOOLS.has(toolName)) {
    const path = stringField(input, "file_path") ?? stringField(input, "notebook_path");
    if (path !== undefined && !isInside(home, path)) {
      return { allow: false, reason: `blocked: ${toolName} outside ${home}` };
    }
    return { allow: true };
  }

  return { allow: true };
}
