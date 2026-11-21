// The operator's voice call. It lives above the chats, so it survives switching views.
import type { DaemonClient, UiEvent } from "../api.ts";
import {
  CallScript,
  ECHO_MS,
  callChanged,
  operatorSaid,
  type CallState,
  type Line,
} from "./call.ts";
import { Speaker, speakable, startLive, type LiveSession } from "./voice.ts";

/** How long the window waits for the last words once the daemon ended the call. */
const GOODBYE_MS = 20_000;

const message = (err: unknown): string => (err instanceof Error ? err.message : String(err));

export class CallSession {
  state: CallState | null = null;
  /** What the operator is saying right now. */
  caption = "";
  /** Whose voice is playing. */
  speaking: string | null = null;
  readonly #client: DaemonClient;
  readonly #toast: (message: string, kind?: "error" | "info") => void;
  readonly #nameOf: (id: string) => string | undefined;
  readonly #speaker: Speaker;
  #script = new CallScript();
  #live: LiveSession | null = null;
  /** A partial of the current utterance already counted as the operator's. */
  #talking = false;
  #seen = new Set<number>();
  /** Bumped when a call starts or closes, so a late microphone or reply is dropped. */
  #round = 0;
  #goodbye: ReturnType<typeof setTimeout> | undefined;
  readonly #subs = new Set<() => void>();

  constructor(
    client: DaemonClient,
    toast: (message: string, kind?: "error" | "info") => void,
    nameOf: (id: string) => string | undefined,
  ) {
    this.#client = client;
    this.#toast = toast;
    this.#nameOf = nameOf;
    this.#speaker = new Speaker(client.speech, (err) => {
      toast(message(err));
    });
    this.#speaker.onSpeaking = (agent) => {
      this.speaking = agent;
      this.#emit();
      this.#closeIfDone();
    };
  }

  /** Calls `fn` on every change; returns the unsubscribe function. */
  subscribe(fn: () => void): () => void {
    this.#subs.add(fn);
    return () => {
      this.#subs.delete(fn);
    };
  }

  /** Starts a call with `agent`; hands-free listening needs ElevenLabs. */
  async start(agent: string): Promise<void> {
    if (this.state) return;
    const round = ++this.#round;
    try {
      const settings = await this.#client.voice();
      const provider = settings.providers.find((p) => p.kind === settings.provider);
      if (!provider?.has_key || !(settings.voice ?? provider.default_voice)) {
        this.#toast("Set up voice first: a key and a voice (Settings → Voice)", "info");
        return;
      }
      const agents = await this.#client.setCall([agent]);
      if (round !== this.#round) return;
      this.#set({
        agents,
        host: agent,
        since: Date.now(),
        muted: false,
        hearing: false,
        ending: false,
      });
      if (settings.provider !== "elevenlabs") {
        this.#toast(
          "Hands-free listening needs ElevenLabs; replies are read aloud, dictate with the microphone",
          "info",
        );
        return;
      }
      await this.#listen(round);
    } catch (err) {
      this.#fail(err);
    }
  }

  /** The operator hangs up: everything stops at once. */
  hangUp(): void {
    const state = this.state;
    this.#close();
    if (state && !state.ending) void this.#client.setCall([]).catch(this.#fail);
  }

  /** Sends silence instead of the operator's microphone, keeping the connection. */
  mute(on: boolean): void {
    if (!this.state) return;
    this.#live?.mute(on);
    this.#set({ ...this.state, muted: on });
  }

  /** The operator's words to everyone in the call; the daemon adds the call's instructions. */
  say(body: string): Promise<unknown> {
    return this.#client.sayInCall(body);
  }

  /** Stops the agents talking without interrupting their turns (the operator dictates). */
  quiet(): void {
    this.#hush(false);
  }

  /** Feeds one live event from the daemon. */
  handle(e: UiEvent): void {
    const state = this.state;
    if (!state) return;
    // An agent that just left still finishes its turn: that is its goodbye.
    const heard = (agent: string, turn: string | null): boolean =>
      state.agents.includes(agent) || (turn !== null && this.#script.has(turn));
    switch (e.type) {
      case "delta":
        if (heard(e.agent_id, e.turn_id))
          this.#say(this.#script.delta(e.agent_id, e.turn_id, e.text));
        break;
      case "event": {
        const ev = e.event;
        const text = (ev.payload as { text?: unknown }).text;
        if (ev.kind === "assistant_text" && ev.turn_id && typeof text === "string") {
          if (!heard(ev.agent_id, ev.turn_id) || this.#seen.has(ev.id)) break;
          this.#seen.add(ev.id);
          this.#say(this.#script.text(ev.agent_id, ev.turn_id, text));
        } else if (ev.kind === "turn_done" && ev.turn_id) {
          this.#say(this.#script.done(ev.turn_id));
        } else if (ev.kind === "runner_disconnected") {
          this.#say(this.#script.leave(ev.agent_id));
        }
        this.#closeIfDone();
        break;
      }
      case "call": {
        const next = callChanged(state, e.agents);
        if (next === state) break;
        if (next?.ending) {
          this.#stopListening();
          this.#goodbye = setTimeout(() => {
            this.#close();
          }, GOODBYE_MS);
        }
        this.#set(next);
        this.#closeIfDone();
        break;
      }
      default:
        break;
    }
  }

  readonly #fail = (err: unknown): void => {
    this.#toast(message(err));
  };

  async #listen(round: number): Promise<void> {
    const live = await startLive((rate) => this.#client.voiceLive("call", rate), {
      partial: (text) => {
        if (!this.#talking && !this.#heard(text)) return;
        this.#talking = true;
        this.caption = text;
        this.#emit();
      },
      committed: (text) => {
        // After a barge-in the cut sentence is recent: a correction reusing its words would read as echo.
        const counted = this.#talking;
        this.#talking = false;
        this.caption = "";
        this.#emit();
        const said = text.trim();
        if (said && (counted || this.#heard(said))) void this.say(said).catch(this.#fail);
      },
      error: (msg) => {
        this.#toast(`Listening stopped: ${msg}`);
        this.#stopListening();
      },
    });
    if (round !== this.#round || !this.state || this.state.ending) {
      void live.stop(false);
      return;
    }
    this.#live = live;
    live.mute(this.state.muted);
    this.#set({ ...this.state, hearing: true });
  }

  #emit(): void {
    for (const fn of this.#subs) fn();
  }

  #set(state: CallState | null): void {
    this.state = state;
    this.#emit();
  }

  #say(lines: Line[]): void {
    for (const line of lines) this.#speaker.say(speakable(line.text, this.#nameOf), line.agent);
  }

  /** Whether the microphone heard the operator; talking over an agent cuts it off. */
  #heard(text: string): boolean {
    const busy = this.#speaker.busy;
    if (!operatorSaid(text, busy, this.#speaker.recent(ECHO_MS))) return false;
    if (busy) this.#hush(true);
    return true;
  }

  #hush(interrupt: boolean): void {
    const agent = this.#speaker.speaking;
    const running = agent !== undefined && this.#script.speaking(agent);
    this.#speaker.stop();
    this.#script.hush();
    if (interrupt && running) void this.#client.interrupt(agent).catch(() => undefined);
  }

  #stopListening(): void {
    void this.#live?.stop(false);
    this.#live = null;
    this.#talking = false;
    this.caption = "";
    if (this.state?.hearing) this.state = { ...this.state, hearing: false };
    this.#emit();
  }

  #closeIfDone(): void {
    if (this.state?.ending && !this.#script.active && !this.#speaker.busy) this.#close();
  }

  #close(): void {
    this.#round++;
    clearTimeout(this.#goodbye);
    this.#stopListening();
    this.#script = new CallScript();
    this.#seen.clear();
    this.state = null;
    this.#speaker.stop();
    this.speaking = null;
    this.#emit();
  }
}
