// Voice in the chat with browser built-ins only: <audio> plays what the
// daemon's speech endpoint returns, MediaRecorder records the operator.
import { mentionsAsText } from "./mentions.ts";

/** Longer replies are cut; the rest stays in the chat. */
export const MAX_SPOKEN = 1500;

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

interface Queued {
  text: string;
  agent: string | undefined;
  audio: Promise<Blob | null> | null;
}

/** Texts fetched ahead of the one playing; more would burst the speech provider. */
const PREFETCH = 2;

/** Plays texts one after another, fetching each one's audio shortly before its turn. */
export class Speaker {
  readonly #fetch: (text: string, agent?: string) => Promise<Blob>;
  readonly #onError: (err: unknown) => void;
  /** Told whose voice starts playing, and `null` once nothing is left to play. */
  onSpeaking: (agent: string | null) => void = () => {};
  #queue: Queued[] = [];
  #audio: HTMLAudioElement | null = null;
  #now: Queued | null = null;
  /** Recently played texts and when they ended, for the call's echo check. */
  #played: { text: string; ended: number }[] = [];
  #round = 0;
  #busy = false;

  constructor(
    fetchAudio: (text: string, agent?: string) => Promise<Blob>,
    onError: (err: unknown) => void,
  ) {
    this.#fetch = fetchAudio;
    this.#onError = onError;
  }

  /** Something is playing or about to. */
  get busy(): boolean {
    return this.#busy;
  }

  /** Whose voice plays now (or loads to play next). */
  get speaking(): string | undefined {
    return this.#now?.agent;
  }

  /** What was played within the last `ms`, including what plays now. */
  recent(ms: number): string {
    const since = Date.now() - ms;
    return [...this.#played.filter((p) => p.ended >= since).map((p) => p.text), this.#now?.text]
      .filter(Boolean)
      .join(" ");
  }

  /** Queues `text`, spoken with `agent`'s voice if it has one. */
  say(text: string, agent?: string): void {
    if (!text) return;
    this.#queue.push({ text, agent, audio: null });
    this.#prefetch();
    if (!this.#busy) void this.#play();
  }

  stop(): void {
    this.#round++;
    this.#queue = [];
    this.#audio?.pause();
    this.#audio = null;
    this.#ended();
    if (this.#busy) this.onSpeaking(null);
    this.#busy = false;
  }

  #prefetch(): void {
    for (const item of this.#queue.slice(0, PREFETCH)) {
      item.audio ??= this.#fetch(item.text, item.agent).catch((err: unknown) => {
        this.#onError(err);
        return null;
      });
    }
  }

  #ended(): void {
    if (this.#now)
      this.#played = [...this.#played.slice(-3), { text: this.#now.text, ended: Date.now() }];
    this.#now = null;
  }

  async #play(): Promise<void> {
    const round = this.#round;
    this.#busy = true;
    while (round === this.#round) {
      this.#prefetch();
      const next = this.#queue.shift();
      if (!next) break;
      this.#prefetch();
      this.#now = next;
      const blob = await next.audio;
      if (round !== this.#round) break;
      if (!blob) {
        this.#now = null;
        continue;
      }
      const url = URL.createObjectURL(blob);
      const audio = new Audio(url);
      this.#audio = audio;
      this.onSpeaking(next.agent ?? null);
      await new Promise<void>((resolve) => {
        audio.onended =
          audio.onerror =
          audio.onpause =
            () => {
              resolve();
            };
        audio.play().catch(() => {
          resolve();
        });
      });
      URL.revokeObjectURL(url);
      if (round === this.#round) this.#ended();
    }
    if (round === this.#round) {
      this.#busy = false;
      this.#audio = null;
      this.onSpeaking(null);
    }
  }
}

export interface Recording {
  /** Stops recording and releases the microphone. */
  stop: () => Promise<Blob>;
}

/** Starts recording the microphone (asks for permission the first time). */
export async function record(): Promise<Recording> {
  if (
    !(navigator.mediaDevices as MediaDevices | undefined) ||
    typeof MediaRecorder === "undefined"
  ) {
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

/** What a live-transcription message means for the UI. */
export type LiveEvent =
  | { kind: "partial"; text: string }
  | { kind: "committed"; text: string }
  | { kind: "error"; message: string }
  | { kind: "other" };

/** Reads one message of ElevenLabs' realtime transcription. */
export function liveEvent(raw: string): LiveEvent {
  let msg: { message_type?: unknown; text?: unknown; error?: unknown };
  try {
    msg = JSON.parse(raw) as typeof msg;
  } catch {
    return { kind: "other" };
  }
  const text = typeof msg.text === "string" ? msg.text : "";
  if (msg.message_type === "partial_transcript") return { kind: "partial", text };
  if (msg.message_type === "committed_transcript") return { kind: "committed", text };
  if (typeof msg.error === "string") return { kind: "error", message: msg.error };
  return { kind: "other" };
}

/** 16-bit little-endian PCM of `samples` (-1..1), base64-encoded. */
export function pcm16Base64(samples: Float32Array): string {
  const bytes = new Uint8Array(samples.length * 2);
  const view = new DataView(bytes.buffer);
  samples.forEach((s, i) => {
    view.setInt16(i * 2, Math.max(-1, Math.min(1, s)) * 0x7fff, true);
  });
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000)
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

const LIVE_RATES = [8000, 16000, 22050, 24000, 44100, 48000];

export interface LiveSession {
  /** Sends silence instead of the microphone (the operator muted it), keeping the connection. */
  mute: (on: boolean) => void;
  /** Ends the session; with `commit`, waits briefly for the last words first. */
  stop: (commit: boolean) => Promise<void>;
}

/**
 * Streams the microphone to a live-transcription WebSocket. `open` gets the
 * sample rate and returns the URL (with a single-use token from the daemon).
 */
export async function startLive(
  open: (sampleRate: number) => Promise<string>,
  on: {
    partial: (text: string) => void;
    committed: (text: string) => void;
    /** The session ended on its own (a provider error or a dropped connection). */
    error: (message: string) => void;
  },
): Promise<LiveSession> {
  if (!(navigator.mediaDevices as MediaDevices | undefined))
    throw new Error("The microphone is not available here");
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
  });
  const release = (): void => {
    for (const track of stream.getTracks()) track.stop();
  };
  let ctx: AudioContext;
  try {
    ctx = new AudioContext({ sampleRate: 16000 });
  } catch {
    ctx = new AudioContext();
  }
  const rate = ctx.sampleRate;
  if (!LIVE_RATES.includes(rate)) {
    release();
    void ctx.close();
    throw new Error(`Unsupported microphone sample rate ${String(rate)} Hz`);
  }
  let ws: WebSocket;
  try {
    ws = new WebSocket(await open(rate));
    await new Promise<void>((resolve, reject) => {
      ws.onopen = () => {
        resolve();
      };
      ws.onerror = () => {
        reject(new Error("Live transcription could not connect"));
      };
    });
  } catch (err) {
    release();
    void ctx.close();
    throw err;
  }
  let lastCommit: (() => void) | null = null;
  let muted = false;
  const send = (samples: Float32Array, commit: boolean): void => {
    if (ws.readyState !== WebSocket.OPEN) return;
    ws.send(
      JSON.stringify({
        message_type: "input_audio_chunk",
        audio_base_64: pcm16Base64(samples),
        commit,
        sample_rate: rate,
      }),
    );
  };
  // ScriptProcessor needs no module file, which the app's CSP would block for an AudioWorklet.
  /* eslint-disable @typescript-eslint/no-deprecated -- see the line above */
  const source = ctx.createMediaStreamSource(stream);
  const proc = ctx.createScriptProcessor(rate >= 32000 ? 8192 : 4096, 1, 1);
  proc.onaudioprocess = (e) => {
    const input = e.inputBuffer.getChannelData(0);
    send(muted ? new Float32Array(input.length) : input, false);
  };
  source.connect(proc);
  proc.connect(ctx.destination);
  /* eslint-enable @typescript-eslint/no-deprecated */

  let ended = false;
  // Once only: a second ctx.close() rejects.
  const end = (): boolean => {
    if (ended) return false;
    ended = true;
    // eslint-disable-next-line @typescript-eslint/no-deprecated -- ScriptProcessor, see above
    proc.onaudioprocess = null;
    source.disconnect();
    proc.disconnect();
    release();
    void ctx.close();
    return true;
  };
  const fail = (message: string): void => {
    if (!end()) return;
    ws.close();
    on.error(message);
  };
  ws.onmessage = (e: MessageEvent<string>) => {
    const ev = liveEvent(e.data);
    if (ev.kind === "partial") on.partial(ev.text);
    else if (ev.kind === "committed") {
      on.committed(ev.text);
      lastCommit?.();
    } else if (ev.kind === "error") fail(ev.message);
  };
  ws.onclose = () => {
    lastCommit?.();
    fail("the connection closed");
  };

  return {
    mute: (on) => {
      muted = on;
    },
    stop: async (commit) => {
      if (!end()) return;
      if (commit && ws.readyState === WebSocket.OPEN) {
        const last = new Promise<void>((resolve) => {
          lastCommit = resolve;
          setTimeout(resolve, 3000);
        });
        send(new Float32Array(Math.round(rate / 10)), true);
        await last;
      }
      ws.close();
    },
  };
}
