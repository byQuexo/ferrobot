// Turn orchestration: one turn at a time, interrupts, session persistence.
import { errorMessage, log } from "./log.ts";
import type { DaemonFrame, RunnerFrame, TurnFrame } from "./protocol.ts";
import type { SessionStore } from "./session.ts";
import { TurnAbortedError, type ModelDriver } from "./drivers/types.ts";

export interface RunnerDeps {
  driver: ModelDriver;
  session: SessionStore;
  agentName: string;
  send: (frame: RunnerFrame) => boolean;
}

export class Runner {
  readonly #deps: RunnerDeps;
  #instructions = "";
  #sessionId: string | null = null;
  #current: { turnId: string; abort: AbortController } | null = null;

  constructor(deps: RunnerDeps, sessionId: string | null) {
    this.#deps = deps;
    this.#sessionId = sessionId;
  }

  get sessionId(): string | null {
    return this.#sessionId;
  }

  get busy(): boolean {
    return this.#current !== null;
  }

  handle(frame: DaemonFrame): void {
    switch (frame.type) {
      case "welcome":
        this.#instructions = frame.instructions;
        this.#sessionId ??= frame.resume_session_id;
        log.info("welcome", { agent: frame.agent_name, resume: this.#sessionId });
        break;
      case "turn":
        if (this.#current) {
          log.warn("turn received while busy; ignoring", { turn: frame.turn_id });
          return;
        }
        void this.#runTurn(frame);
        break;
      case "configure":
        // Applies from the next turn; a running turn keeps its prompt.
        this.#instructions = frame.instructions;
        log.info("instructions updated");
        break;
      case "interrupt":
        if (this.#current?.turnId === frame.turn_id) {
          log.info("interrupting turn", { turn: frame.turn_id });
          this.#current.abort.abort();
        }
        break;
    }
  }

  /** The daemon redelivers in-flight messages, so a lost link aborts the turn. */
  abortCurrent(): void {
    this.#current?.abort.abort();
  }

  async #runTurn(turn: TurnFrame): Promise<void> {
    const abort = new AbortController();
    this.#current = { turnId: turn.turn_id, abort };
    const send = (frame: RunnerFrame): void => {
      if (!this.#deps.send(frame)) log.debug("frame dropped (offline)", { type: frame.type });
    };
    let done: RunnerFrame;
    try {
      const out = await this.#deps.driver.runTurn({
        turn,
        instructions: this.#instructions,
        agentName: this.#deps.agentName,
        sessionId: this.#sessionId,
        signal: abort.signal,
        emit: (event) => { send({ type: "event", turn_id: turn.turn_id, event }); },
      });
      if (out.sessionId && out.sessionId !== this.#sessionId) {
        this.#sessionId = out.sessionId;
        await this.#deps.session.save(out.sessionId);
      }
      done = {
        type: "turn_done",
        turn_id: turn.turn_id,
        outcome: abort.signal.aborted ? "interrupted" : "completed",
        session_id: this.#sessionId,
        result: out.result,
        error: null,
      };
    } catch (err) {
      const interrupted = err instanceof TurnAbortedError || abort.signal.aborted;
      if (!interrupted) log.error("turn failed", { turn: turn.turn_id, error: errorMessage(err) });
      done = {
        type: "turn_done",
        turn_id: turn.turn_id,
        outcome: interrupted ? "interrupted" : "failed",
        session_id: this.#sessionId,
        result: null,
        error: interrupted ? null : errorMessage(err),
      };
    } finally {
      this.#current = null;
    }
    send(done);
  }
}
