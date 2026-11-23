import type { AvatarCharacter, Presence } from "../api.ts";
import { hash } from "./avatar.ts";

export type Character = AvatarCharacter;
export type Body = Character["body"];
export type Eyes = Character["eyes"];
export type Mood = "idle" | "thinking" | "working" | "sleeping" | "trouble" | "waiting";

export const BODIES: readonly Body[] = ["blob", "bean", "pebble", "sprout"];
export const EYES: readonly Eyes[] = ["dots", "ovals", "wide", "visor"];
/** Hues that read well on light and dark backgrounds. */
export const HUES: readonly number[] = [8, 28, 46, 96, 152, 176, 200, 222, 248, 276, 304, 336];

/** The deterministic look for an agent id (or any seed). */
export function deriveCharacter(seed: string): Character {
  const h = hash(seed);
  return {
    body: BODIES[h % BODIES.length] ?? "blob",
    eyes: EYES[(h >>> 4) % EYES.length] ?? "dots",
    hue: HUES[(h >>> 8) % HUES.length] ?? 222,
  };
}

/** The stored look if set, else the derived one. */
export function characterOf(agent: {
  id?: string;
  name: string;
  character?: Character | null;
}): Character {
  return agent.character ?? deriveCharacter(agent.id ?? agent.name);
}

/** A random look different from `current` (for "re-roll"). */
export function rerollCharacter(current: Character, random: () => number = Math.random): Character {
  for (;;) {
    const pick = <T>(xs: readonly T[]): T => xs[Math.floor(random() * xs.length)] as T;
    const next = { body: pick(BODIES), eyes: pick(EYES), hue: pick(HUES) };
    if (next.body !== current.body || next.eyes !== current.eyes || next.hue !== current.hue)
      return next;
  }
}

/** A problem, then an open question card, outrank what the agent is doing. */
export function moodOf(
  presence: Presence | undefined,
  flags: { trouble?: boolean; waiting?: boolean } = {},
): Mood {
  if (presence === "error" || flags.trouble) return "trouble";
  if (flags.waiting) return "waiting";
  switch (presence) {
    case "working":
      return "working";
    case "starting":
      return "thinking";
    case "sleeping":
      return "sleeping";
    default:
      return "idle";
  }
}

/** Blink offset in seconds, so a sidebar full of agents doesn't blink in sync. */
export function blinkDelay(seed: string): number {
  return (hash(`blink:${seed}`) % 40) / 10;
}
