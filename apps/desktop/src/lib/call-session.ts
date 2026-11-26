// The operator's voice call. It lives above the chats, so it survives switching views.
import type { DaemonClient, UiEvent } from "../api.ts";
import {
  CallScript,
  ECHO_MS,
  callChanged,
  callTurn,
  clampVolume,
  loadPushToTalk,
  loadVolume,
  micOpen,
  operatorSaid,
  savePushToTalk,
  saveVolume,
  type CallState,
  type Line,
} from "./call.ts";
import { errorText } from "./format.ts";
import { Speaker, speakable, startLive, type LiveSession } from "./voice.ts";

/** How long the window waits for the last words once the daemon ended the call. */
const GOODBYE_MS = 20_000;
/** The microphone stays open this long after push-to-talk is let go: audio goes out in chunks. */
const RELEASE_MS = 300;

export class CallSession {
  state: CallState | null = null;
  /** What the operator is saying right now. */
  caption = "";
  /** Whose voice is playing. */
  speaking: string | null = null;
  /** The operator's microphone level (RMS, 0..1); 0 while muted or not listening. */
  level = 0;
  /** The microphone counts only while the operator holds the talk button or Space. */
  pushToTalk = loadPushToTalk();
  held = false;
  /** How loud the agents speak (0..1); remembered on this computer. */
  volume = loadVolume();
  #release: ReturnType<typeof setTimeout> | undefined;
  readonly #client: DaemonClient;
  readonly #toast: (message: string, kind?: "error" | "info") => void;
  readonly #nameOf: (id: string) => string | undefined;
  readonly #speaker: Speaker;
  #script = new CallScript();
  #live: LiveSession | null = null;
  /** A partial of the current utterance already counted as the operator's. */
  #talking = false;
  #seen = new Set<number>();
  /** Turns that are part of the call (see `callTurn`) and whose agent; only these are spoken. */
  #turns = new Map<string, string>();
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
    this.#speaker = new Speaker(client.speechUrl, (err) => {
      toast(errorText(err));
    });
    this.#speaker.setVolume(this.volume);
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

  /** Starts a call with `agent`, or with every member of `group`; hands-free listening needs
   * ElevenLabs. */
  /** Calls `agent`, or the group `group`; with `join`, joins the call agents started in room `group`. */
  async start(agent: string, group?: number, join = false): Promise<void> {
    if (this.state) return;
    const round = ++this.#round;
    try {
      const settings = await this.#client.voice();
      const provider = settings.providers.find((p) => p.kind === settings.provider);
      if (!provider?.has_key || !(settings.voice ?? provider.default_voice)) {
        this.#toast("Set up voice first: a key and a voice (Settings → Voice)", "info");
        return;
      }
      const view = await (group === undefined
        ? this.#client.setCall([agent])
        : join
          ? this.#client.joinRoom(group)
          : this.#client.callGroup(group));
      if (round !== this.#round) return;
      this.#set({
        agents: view.agents,
        host: view.agents[0] ?? agent,
        since: Date.now(),
        muted: false,
        hearing: false,
        ending: false,
        room: view.room_id ?? null,
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
    this.#set({ ...this.state, muted: on });
    this.#gate();
  }

  /** Switches between push-to-talk and voice activity; remembered on this computer. */
  setPushToTalk(on: boolean): void {
    this.pushToTalk = on;
    savePushToTalk(on);
    this.held = false;
    clearTimeout(this.#release);
    this.#gate();
    this.#emit();
  }

  /** How loud the agents speak, 0 (silent) to 1; remembered on this computer. */
  setVolume(v: number): void {
    this.volume = clampVolume(v);
    saveVolume(this.volume);
    this.#speaker.setVolume(this.volume);
    this.#emit();
  }

  /** Push-to-talk: the operator presses (`true`) or lets go of the talk control. */
  talk(held: boolean): void {
    if (!this.pushToTalk || held === this.held || (held && this.state?.muted)) return;
    this.held = held;
    clearTimeout(this.#release);
    if (held) this.#gate();
    else
      this.#release = setTimeout(() => {
        this.#gate();
      }, RELEASE_MS);
    this.#emit();
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
    switch (e.type) {
      case "delta":
        if (this.#turns.has(e.turn_id))
          this.#say(this.#script.delta(e.agent_id, e.turn_id, e.text));
        break;
      case "event": {
        const ev = e.event;
        const { text, room_id: room } = ev.payload as { text?: unknown; room_id?: unknown };
        if (ev.kind === "message_in" && ev.turn_id && !state.ending) {
          if (callTurn(state, ev.agent_id, typeof room === "number" ? room : null))
            this.#turns.set(ev.turn_id, ev.agent_id);
        } else if (ev.kind === "assistant_text" && ev.turn_id && typeof text === "string") {
          // An agent that just left still finishes its turn: that is its goodbye.
          if (!this.#turns.has(ev.turn_id) || this.#seen.has(ev.id)) break;
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
        const next = callChanged(state, e.agents, e.room_id ?? null);
        if (next === state) break;
        if (next?.ending) {
          this.#stopListening();
          this.#onlyGoodbye(e.hung_up ?? null);
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
    this.#toast(errorText(err));
  };

  /** The daemon ended the call: only the goodbye of the agent that hung up (`keep`) is heard. */
  #onlyGoodbye(keep: string | null): void {
    for (const [turn, agent] of this.#turns) if (agent !== keep) this.#turns.delete(turn);
    const lines = this.#script.end(keep);
    if (keep === null) {
      this.#speaker.stop();
    } else {
      this.#speaker.keepOnly(keep);
      this.#say(lines);
    }
  }

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
      level: (rms) => {
        this.level = rms;
        this.#emit();
      },
    });
    if (round !== this.#round || !this.state || this.state.ending) {
      void live.stop(false);
      return;
    }
    this.#live = live;
    this.#gate();
    this.#set({ ...this.state, hearing: true });
  }

  #gate(): void {
    if (this.state) this.#live?.mute(!micOpen(this.state.muted, this.pushToTalk, this.held));
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
    this.held = false;
    clearTimeout(this.#release);
    this.caption = "";
    this.level = 0;
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
    this.#turns.clear();
    this.state = null;
    this.#speaker.stop();
    this.speaking = null;
    this.#emit();
  }
}
