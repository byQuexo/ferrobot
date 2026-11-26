import type { Flow, FlowRun, RunStatus, Step } from "../api.ts";

export const RUN_LABELS: Record<RunStatus, string> = {
  waiting: "Waiting",
  running: "Running",
  done: "Done",
  stopped: "Stopped",
  failed: "Failed",
};

export function flowStepLabel(step: Step, nameOf: (id: string) => string): string {
  switch (step.kind) {
    case "agent":
      return `${nameOf(step.agent)}: ${step.prompt}`;
    case "condition":
      return `if it contains “${step.contains}”`;
    case "message":
      return `message to ${nameOf(step.agent)}`;
  }
}

export function newStep(kind: Step["kind"], agent: string): Step {
  if (kind === "condition") return { kind, contains: "" };
  return kind === "agent" ? { kind, agent, prompt: "" } : { kind, agent };
}

export const blankFlow = (agent: string): Flow => ({
  id: crypto.randomUUID(),
  name: "",
  steps: [newStep("agent", agent)],
});

/** `runs` with `run` updated in place, or added on top. */
export function upsertRun(runs: FlowRun[], run: FlowRun): FlowRun[] {
  return runs.some((r) => r.id === run.id)
    ? runs.map((r) => (r.id === run.id ? run : r))
    : [run, ...runs].slice(0, 50);
}
