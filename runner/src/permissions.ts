// Guard rails inside the sandbox, which stays the real security boundary: obviously destructive
// shell commands are refused and file tools stay inside the agent's workspace.
import { isAbsolute, relative, resolve } from "node:path";

export type Decision = { allow: true } | { allow: false; reason: string };

const DESTRUCTIVE_SHELL: readonly { pattern: RegExp; reason: string }[] = [
  {
    pattern:
      /\brm\s+(?:-\S*\s+)*?(?:-[a-zA-Z]*[rR][a-zA-Z]*|--recursive)\s+(?:-\S*\s+)*["']?(?:\/|~\/?|\$HOME\/?|\$\{HOME\}\/?)\*?["']?(?=[\s;&|]|$)/,
    reason: "recursive delete of / or $HOME",
  },
  { pattern: /\bmkfs(\.\w+)?\b/, reason: "formatting a filesystem" },
  { pattern: /:\(\)\s*\{\s*:\|:&\s*\};:/, reason: "fork bomb" },
  {
    pattern: /\b(curl|wget)\b[^|]*\|\s*(sudo\s+)?(ba|z)?sh\b/,
    reason: "piping a download into a shell",
  },
  { pattern: /\bdd\s+[^\n]*of=\/dev\//, reason: "writing to a raw device" },
];

const FILE_TOOLS = new Set(["Read", "Write", "Edit", "MultiEdit", "NotebookEdit"]);

/** Tool groups built into the model's harness; the runner turns them off. */
export type BuiltinGroup = "shell" | "files" | "web";
const BUILTIN_GROUPS: readonly BuiltinGroup[] = ["shell", "files", "web"];

/** The Claude Agent SDK's built-in tools per group (checked against SDK 0.3.328's tool list). */
export const CLAUDE_TOOLS: Readonly<Record<BuiltinGroup, readonly string[]>> = {
  shell: ["Bash", "Monitor", "PowerShell"],
  files: [
    "Read",
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "Glob",
    "Grep",
    "LSP",
    "EnterWorktree",
    "ExitWorktree",
  ],
  web: ["WebFetch", "WebSearch"],
};
/** Sub-agents run their own tools, so they go whenever a built-in group is off. */
export const CLAUDE_SUBAGENT_TOOLS: readonly string[] = ["Agent", "Workflow"];

/** The built-in groups a profile turns off; no profile: none. */
export function offGroups(tools: readonly string[] | undefined): ReadonlySet<BuiltinGroup> {
  return new Set(tools ? BUILTIN_GROUPS.filter((g) => !tools.includes(g)) : []);
}

function groupOf(toolName: string): BuiltinGroup | undefined {
  return BUILTIN_GROUPS.find((g) => CLAUDE_TOOLS[g].includes(toolName));
}

function isInside(root: string, target: string): boolean {
  const rel = relative(resolve(root), resolve(root, target));
  return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
}

function stringField(input: Record<string, unknown>, key: string): string | undefined {
  const value = input[key];
  return typeof value === "string" ? value : undefined;
}

/** Decides whether a tool call may run (Claude's tool names); file tools must stay inside
 * `home` and tools of `off` groups never run. */
export function decide(
  toolName: string,
  input: Record<string, unknown>,
  home: string,
  off: ReadonlySet<BuiltinGroup> = new Set(),
): Decision {
  if (toolName.startsWith("mcp__ferrobot__")) return { allow: true };
  const group = groupOf(toolName);
  if (group && off.has(group)) {
    return { allow: false, reason: `blocked: ${group} is turned off for this agent` };
  }
  if (off.size > 0 && CLAUDE_SUBAGENT_TOOLS.includes(toolName)) {
    return { allow: false, reason: "blocked: sub-agents are off while a built-in tool is off" };
  }

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
