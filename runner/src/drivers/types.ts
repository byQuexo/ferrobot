import type { TurnEvent, TurnFrame } from "../protocol.ts";

export interface TurnContext {
  turn: TurnFrame;
  /** Operator-provided role of this agent. */
  instructions: string;
  agentName: string;
  /** Session to resume, if any. */
  sessionId: string | null;
  signal: AbortSignal;
  emit: (event: TurnEvent) => void;
}

export interface TurnResult {
  /** Final assistant text. */
  result: string | null;
  sessionId: string | null;
}

/** A model backend. One turn at a time; must honour `signal`. */
export interface ModelDriver {
  readonly name: string;
  runTurn(ctx: TurnContext): Promise<TurnResult>;
  close(): Promise<void>;
}

export class TurnAbortedError extends Error {
  override name = "TurnAbortedError";
}
