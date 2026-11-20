// Voice in the chat with browser built-ins only: <audio> plays what the
// daemon's speech endpoint returns, MediaRecorder records the operator.
import { mentionsAsText } from "./mentions.ts";

/** Longer replies are cut; the rest stays in the chat. */
const MAX_SPOKEN = 1500;

/** What to read aloud for a chat message: no code, links or Markdown marks. */
export function speakable(text: string, nameOf: (id: string) => string | undefined): string {
  let t = mentionsAsText(text, nameOf)
    .replace(/```[\s\S]*?(```|$)/g, " (code) ")
    .replace(/`([^`\n]*)`/g, "$1")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/https?:\/\/\S+/g, "(link)")
    .replace(/^\s*([-*_]\s*){3,}$/gm, "")
    .replace(/^\s{0,3}(#{1,6}|>|[-*+]|\d+[.)])\s+/gm, "")
    .replace(/(\*{1,3}|~~)(\S(?:.*?\S)?)\1/g, "$2")
    .replace(/(^|\s)_(\S(?:.*?\S)?)_(?=\s|$|[.,!?;:])/g, "$1$2")
    .replace(/\|/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (t.length > MAX_SPOKEN) {
    const cut = t.lastIndexOf(". ", MAX_SPOKEN);
    t = `${t.slice(0, cut > 0 ? cut + 1 : MAX_SPOKEN)} The rest is in the chat.`;
  }
  return t;
}

/** Plays texts one after another; each one's audio is fetched as soon as it is queued. */
export class Speaker {
  readonly #fetch: (text: string) => Promise<Blob>;
  readonly #onError: (err: unknown) => void;
  #queue: Promise<Blob | null>[] = [];
  #audio: HTMLAudioElement | null = null;
  #round = 0;
  #playing = false;

  constructor(fetchAudio: (text: string) => Promise<Blob>, onError: (err: unknown) => void) {
    this.#fetch = fetchAudio;
    this.#onError = onError;
  }

  say(text: string): void {
    if (!text) return;
    this.#queue.push(this.#fetch(text).catch((err: unknown) => {
      this.#onError(err);
      return null;
    }));
    if (!this.#playing) void this.#play();
  }

  stop(): void {
    this.#round++;
    this.#queue = [];
    this.#audio?.pause();
    this.#audio = null;
    this.#playing = false;
  }

  async #play(): Promise<void> {
    const round = this.#round;
    this.#playing = true;
    for (let next = this.#queue.shift(); next && round === this.#round; next = this.#queue.shift()) {
      const blob = await next;
      if (!blob || round !== this.#round) continue;
      const url = URL.createObjectURL(blob);
      const audio = new Audio(url);
      this.#audio = audio;
      await new Promise<void>((resolve) => {
        audio.onended = audio.onerror = audio.onpause = () => { resolve(); };
        audio.play().catch(() => { resolve(); });
      });
      URL.revokeObjectURL(url);
    }
    if (round === this.#round) {
      this.#playing = false;
      this.#audio = null;
    }
  }
}

export interface Recording {
  /** Stops recording and releases the microphone. */
  stop: () => Promise<Blob>;
}

/** Starts recording the microphone (asks for permission the first time). */
export async function record(): Promise<Recording> {
  if (!(navigator.mediaDevices as MediaDevices | undefined) || typeof MediaRecorder === "undefined") {
    throw new Error("The microphone is not available here");
  }
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const recorder = new MediaRecorder(stream);
  const chunks: Blob[] = [];
  recorder.ondataavailable = (e) => {
    if (e.data.size > 0) chunks.push(e.data);
  };
  recorder.start();
  return {
    stop: () =>
      new Promise((resolve) => {
        recorder.onstop = () => {
          for (const track of stream.getTracks()) track.stop();
          resolve(new Blob(chunks, { type: recorder.mimeType || chunks[0]?.type || "audio/webm" }));
        };
        recorder.stop();
      }),
  };
}
