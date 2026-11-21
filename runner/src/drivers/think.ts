// Some models stream their reasoning into the reply itself (`<think>…</think>`, or
// `Thinking: …</think:abc123>`). Only what follows it is the answer.

const OPENERS = ["<think", "thinking:"];
const CLOSER = /<\/think[^>]*>/i;

/** Holds back a reply's leading reasoning so only the answer is shown and spoken. */
export class ThinkFilter {
  #held = "";
  #state: "deciding" | "thinking" | "answer" = "deciding";
  #opener = "";

  /** The part of `piece` that belongs to the answer (often ""). */
  push(piece: string): string {
    if (this.#state === "answer") return piece;
    this.#held += piece;
    if (this.#state === "deciding") {
      const head = this.#held.trimStart().toLowerCase();
      const opener = OPENERS.find((o) => head.startsWith(o));
      if (opener) {
        this.#state = "thinking";
        this.#opener = opener;
      } else if (head === "" || OPENERS.some((o) => o.startsWith(head))) {
        return "";
      } else {
        return this.#release(this.#held);
      }
    }
    const close = CLOSER.exec(this.#held);
    if (!close) return "";
    return this.#release(this.#held.slice(close.index + close[0].length).trimStart());
  }

  /** What is still held when the reply ends: reasoning that never closed is dropped. */
  finish(): string {
    if (this.#state === "answer") return "";
    const held = this.#held;
    this.#held = "";
    // An unclosed `<think>` is all reasoning; a bare "Thinking:" may just be the answer.
    if (this.#state === "thinking" && this.#opener === "<think") return "";
    this.#state = "answer";
    return held;
  }

  #release(text: string): string {
    this.#state = "answer";
    this.#held = "";
    return text;
  }
}
