import type { SteerFrame, TurnEvent, TurnFrame } from "../protocol.ts";

export interface TurnContext {
  turn: TurnFrame;
  /** Operator-provided role of this agent. */
  instructions: string;
  agentName: string;
  /** Session to resume, if any. */
  sessionId: string | null;
  signal: AbortSignal;
  emit: (event: TurnEvent) => void;
  /** Messages that arrived during this turn, for drivers with `steering`;
   * each call hands them over once and marks them as seen by the model. */
  takeMessages?: () => SteerFrame[];
}

export interface TurnResult {
  /** Final assistant text. */
  result: string | null;
  sessionId: string | null;
}

/** A model backend. One turn at a time; must honour `signal`. */
export interface ModelDriver {
  readonly name: string;
  /** Can add messages that arrive mid-turn (`TurnContext.takeMessages`). */
  readonly steering?: boolean;
  runTurn(ctx: TurnContext): Promise<TurnResult>;
  close(): Promise<void>;
}

export class TurnAbortedError extends Error {
  override name = "TurnAbortedError";
}
