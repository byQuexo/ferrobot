// What happens in the operator's voice call: which agent lines to speak and when,
// what counts as the operator talking, and who is in the call.
import { MAX_SPOKEN } from "./voice.ts";

/** One piece of an agent's reply to read aloud with its voice. */
export interface Line {
  agent: string;
  text: string;
}

/** The agents' replies cut this long ago may still come back through the microphone. */
export const ECHO_MS = 2000;
/** Share of heard words that were just spoken, from which it counts as echo. */
const ECHO_SHARE = 0.6;

const ABBREVIATIONS = new Set([
  "mr",
  "mrs",
  "ms",
  "dr",
  "prof",
  "sr",
  "jr",
  "st",
  "vs",
  "etc",
  "approx",
  "ca",
  "bzw",
  "usw",
  "vgl",
  "nr",
  "inc",
  "ltd",
  "fig",
]);

/** Whether the `.` at `dot` ends a sentence (not "e.g.", "Dr." or "3. May"). */
function endsSentence(text: string, dot: number): boolean {
  const word = (/(\S+)$/.exec(text.slice(0, dot))?.[1] ?? "").replace(/^[("'“‘[]+/, "");
  if (/^\d{1,2}$/.test(word)) return false;
  if (/^\p{L}(\.\p{L})*$/u.test(word)) return false;
  return !ABBREVIATIONS.has(word.toLowerCase());
}

const fences = (s: string): number => s.split("```").length - 1;

/**
 * A reply's first words up to a clause break (`,` `;` `:` or a spaced dash) once there are
 * at least 20 characters, so speech can start before the first sentence is written.
 */
export function firstClause(text: string): { clause: string; rest: string } | null {
  if (text.includes("```")) return null;
  const m = /^([\s\S]{20,}?(?:[,;:]|\s[–—-]))\s/.exec(text);
  return m?.[1] ? { clause: m[1].trim(), rest: text.slice(m[0].length) } : null;
}

/**
 * Splits streamed text into whole sentences and the unfinished rest. A sentence
 * ends at `.`, `!`, `?` or `…` followed by whitespace, or at a line break; never
 * inside a code block.
 */
export function splitSentences(text: string): { done: string[]; rest: string } {
  const done: string[] = [];
  let start = 0;
  for (const m of text.matchAll(/[.!?…]+["'”’)\]]*(?=\s)|\n/g)) {
    const at = m.index + m[0].length;
    if (m[0] !== "\n" && /^\.["'”’)\]]*$/.test(m[0]) && !endsSentence(text, m.index)) continue;
    if (fences(text.slice(start, at)) % 2) continue;
    const sentence = text.slice(start, at).trim();
    if (sentence) done.push(sentence);
    start = at;
  }
  return { done, rest: text.slice(start) };
}

/** An agent's "nothing to add" reply in a call: never spoken. */
export const isPass = (text: string): boolean => /^\(?pass\)?[.!]?$/i.test(text.trim());

const words = (s: string): string[] => s.toLowerCase().match(/[\p{L}\p{N}']+/gu) ?? [];

/**
 * Whether the microphone heard the operator, not the agents' own voices coming back.
 * While an agent speaks (`busy`), it takes two words to interrupt; `recent` is what
 * was spoken in the last moments, and words that mostly repeat it are echo.
 */
export function operatorSaid(heard: string, busy: boolean, recent: string): boolean {
  const said = words(heard);
  if (said.length === 0 || (busy && said.length < 2)) return false;
  if (said.length < 2 || !recent) return true;
  const echo = new Set(words(recent));
  return said.filter((w) => echo.has(w)).length / said.length < ECHO_SHARE;
}

interface Turn {
  agent: string;
  /** Streamed text not split into sentences yet. */
  rest: string;
  /** Sentences held back while another agent speaks. */
  waiting: string[];
  streamed: boolean;
  /** Characters handed out to be spoken. */
  said: number;
  /** Silenced by the operator or past the length limit. */
  quiet: boolean;
  /** Its current text block ended (`assistant_text`). */
  ended: boolean;
  /** `turn_done` arrived; forgotten once its waiting sentences are out. */
  closed: boolean;
}

/**
 * Turns streamed replies into lines to speak: sentence by sentence as they arrive,
 * each text once, and one agent's text block at a time so voices never interleave.
 */
export class CallScript {
  #turns = new Map<string, Turn>();
  /** The turn whose block is being spoken; the others wait. */
  #floor: string | null = null;
  #next: string[] = [];

  /** Streamed text (`delta`) of `turn`. */
  delta(agent: string, turn: string, text: string): Line[] {
    const t = this.#turn(agent, turn);
    t.streamed = true;
    t.ended = false;
    return this.#add(turn, t, text);
  }

  /** A finished text block (`assistant_text`); a streamed turn already had its words. */
  text(agent: string, turn: string, text: string): Line[] {
    const t = this.#turn(agent, turn);
    const out = t.streamed ? [] : this.#add(turn, t, text);
    return [...out, ...this.#endBlock(turn, t)];
  }

  /** The turn is over (`turn_done`): what is left is spoken, then it is forgotten. */
  done(turn: string): Line[] {
    const t = this.#turns.get(turn);
    if (!t) return [];
    const out = this.#endBlock(turn, t);
    t.closed = true;
    if (t.waiting.length === 0) this.#turns.delete(turn);
    return out;
  }

  /** Ends every turn of `agent` (its runner went away). */
  leave(agent: string): Line[] {
    return [...this.#turns]
      .filter(([, t]) => t.agent === agent && !t.closed)
      .flatMap(([id]) => this.done(id));
  }

  /** Whether `agent` has a turn still running. */
  speaking(agent: string): boolean {
    return [...this.#turns.values()].some((t) => t.agent === agent && !t.closed);
  }

  /** Whether a turn still running may say more. */
  get active(): boolean {
    return [...this.#turns.values()].some((t) => !t.closed && !t.quiet);
  }

  /**
   * The call ended: every turn but `keep`'s (the agent that hung up) goes quiet at once.
   * Returns `keep`'s lines that waited for another voice to finish.
   */
  end(keep: string | null): Line[] {
    for (const [id, t] of this.#turns) {
      if (t.agent === keep) continue;
      t.quiet = true;
      t.rest = "";
      t.waiting = [];
      if (t.closed) this.#turns.delete(id);
    }
    this.#next = this.#next.filter((id) => this.#turns.get(id)?.agent === keep);
    const floor = this.#floor === null ? undefined : this.#turns.get(this.#floor);
    return floor !== undefined && floor.agent === keep ? [] : this.#pass();
  }

  /** The operator cut in: drops what was not spoken and keeps those turns quiet. */
  hush(): void {
    for (const [id, t] of this.#turns) {
      if (t.said === 0 && t.waiting.length === 0 && !t.rest.trim()) continue;
      t.quiet = true;
      t.rest = "";
      t.waiting = [];
      if (t.closed) this.#turns.delete(id);
    }
    this.#floor = null;
    this.#next = [];
  }

  #turn(agent: string, turn: string): Turn {
    let t = this.#turns.get(turn);
    if (!t) {
      t = {
        agent,
        rest: "",
        waiting: [],
        streamed: false,
        said: 0,
        quiet: false,
        ended: false,
        closed: false,
      };
      this.#turns.set(turn, t);
    }
    return t;
  }

  #add(turn: string, t: Turn, text: string): Line[] {
    if (t.quiet) return [];
    let { done, rest } = splitSentences(t.rest + text);
    if (t.said === 0 && done.length === 0) {
      const first = firstClause(rest);
      if (first) [done, rest] = [[first.clause], first.rest];
    }
    t.rest = rest;
    return this.#offer(turn, t, done);
  }

  #endBlock(turn: string, t: Turn): Line[] {
    const out = t.quiet ? [] : this.#offer(turn, t, [t.rest]);
    t.rest = "";
    t.ended = true;
    return this.#floor === turn ? [...out, ...this.#pass()] : out;
  }

  #offer(turn: string, t: Turn, sentences: string[]): Line[] {
    const texts: string[] = [];
    for (const raw of sentences) {
      const text = raw.trim();
      if (!text || isPass(text) || t.quiet) continue;
      if (t.said + text.length > MAX_SPOKEN) {
        t.quiet = true;
        // A first sentence this long is cut by `speakable`, which adds the same notice.
        texts.push(t.said ? "The rest is in the chat." : text);
        break;
      }
      t.said += text.length;
      texts.push(text);
    }
    if (texts.length === 0) return [];
    this.#floor ??= turn;
    if (this.#floor === turn) return texts.map((text) => ({ agent: t.agent, text }));
    t.waiting.push(...texts);
    if (!this.#next.includes(turn)) this.#next.push(turn);
    return [];
  }

  /** Hands the floor to the next waiting turn; one whose block already ended passes it on. */
  #pass(): Line[] {
    this.#floor = null;
    const out: Line[] = [];
    while (this.#floor === null) {
      const id = this.#next.shift();
      if (id === undefined) break;
      const t = this.#turns.get(id);
      if (!t) continue;
      out.push(...t.waiting.map((text) => ({ agent: t.agent, text })));
      t.waiting = [];
      if (t.closed) this.#turns.delete(id);
      else if (!t.ended) this.#floor = id;
    }
    return out;
  }
}

/** The operator's call as the UI keeps it. */
export interface CallState {
  agents: string[];
  /** Whose chat the call belongs to. */
  host: string;
  /** When it started (ms since the epoch). */
  since: number;
  muted: boolean;
  /** Live listening runs (ElevenLabs only). */
  hearing: boolean;
  /** The daemon ended the call; the last words are still being spoken. */
  ending: boolean;
  /** The call's room, once a second agent joined. */
  room: number | null;
}

/** Applies a `call` event: an agent joined or left, or (empty) the call ended. */
export function callChanged(
  state: CallState | null,
  agents: string[],
  room: number | null = null,
): CallState | null {
  if (!state || state.ending) return state;
  if (agents.length === 0) return { ...state, agents, hearing: false, ending: true };
  const host = agents.includes(state.host) ? state.host : (agents[0] ?? state.host);
  return { ...state, agents, host, room };
}

/**
 * Whether a turn is part of the call, judged when it starts (its `message_in`, with the room it
 * came from): in a room only the room's turns, so members' own chats stay silent; without one
 * the host's turns.
 */
export function callTurn(state: CallState, agent: string, room: number | null): boolean {
  return state.room !== null ? room === state.room : room === null && agent === state.host;
}

/** A room's name from who took part, e.g. `Call · ann, bob`; agents deleted since are left out. */
export function roomTitle(members: string[], nameOf: (id: string) => string | undefined): string {
  const names = members.flatMap((id) => nameOf(id) ?? []);
  return names.length ? `Call · ${names.join(", ")}` : "Call";
}

/** A group edit for `PATCH /v1/groups/{id}`: members only when they changed, since a group in a
 * call refuses member changes. */
export function groupChange(
  before: string[],
  name: string,
  members: string[],
): { name: string; members?: string[] } {
  const same = members.length === before.length && members.every((id, i) => id === before[i]);
  return same ? { name: name.trim() } : { name: name.trim(), members };
}

/** A running call's length, e.g. `0:42` or `1:05:03`. */
export function callClock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const two = (n: number): string => String(n).padStart(2, "0");
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return h ? `${String(h)}:${two(m)}:${two(s % 60)}` : `${String(m)}:${two(s % 60)}`;
}
