// Slash commands in the message box: `/task` and the operator's own (Settings → Commands).
import type { SlashCommand } from "../api.ts";

export const TASK_COMMAND: SlashCommand = {
  name: "task",
  description: "Give the agent a goal: it keeps working until it is reached",
  prompt: "$ARGUMENTS",
  as_task: true,
};

/** The command name being typed at the very start of the box, before any space. */
export function commandQuery(text: string, caret: number): string | null {
  const m = /^\/([a-z0-9-]*)$/.exec(text.slice(0, caret));
  return m ? (m[1] ?? "") : null;
}

/** Commands whose name starts with `query`, `/task` first. */
export function matchCommands(commands: readonly SlashCommand[], query: string): SlashCommand[] {
  return [TASK_COMMAND, ...commands].filter((c) => c.name.startsWith(query)).slice(0, 8);
}

/** The prompt with `$ARGUMENTS` filled in; without it, the arguments follow after a blank line. */
export function fillPrompt(prompt: string, args: string): string {
  if (prompt.includes("$ARGUMENTS")) return prompt.replaceAll("$ARGUMENTS", args).trim();
  return args ? `${prompt.trimEnd()}\n\n${args}` : prompt.trim();
}

/** What `/name rest` sends, when `name` is a command; `null` sends the text as typed. */
export function expandCommand(
  text: string,
  commands: readonly SlashCommand[],
): { name: string; body: string; asTask: boolean } | null {
  const m = /^\/([a-z][a-z0-9-]*)(?:\s+([\s\S]*))?$/.exec(text.trim());
  const command = m && [TASK_COMMAND, ...commands].find((c) => c.name === m[1]);
  if (!command) return null;
  const body = fillPrompt(command.prompt, (m[2] ?? "").trim());
  return { name: command.name, body, asTask: command.as_task === true };
}
