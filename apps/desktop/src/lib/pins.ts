import type { Pin, PinKind } from "../api.ts";
import { mentionsAsText } from "./mentions.ts";

const MAX_LABEL = 80;

/** A reply's default pin label: its first words as plain text, mentions by current name. */
export function pinLabel(text: string, nameOf: (id: string) => string | undefined): string {
  const plain = mentionsAsText(text, nameOf)
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/^\s*(?:#+|>|[-*+]|\d+\.)\s+/gm, "")
    .replace(/[*_`]/g, "")
    .replace(/\s+/g, " ")
    .trim();
  const label = plain || "Reply";
  return label.length > MAX_LABEL ? `${label.slice(0, MAX_LABEL - 1).trimEnd()}…` : label;
}

export const pinKey = (p: Pick<Pin, "kind" | "agent_id" | "event">): string =>
  `${p.kind}:${p.agent_id}:${String(p.event)}`;

export function isPinned(pins: Pin[], kind: PinKind, agent: string, event: number): boolean {
  const key = pinKey({ kind, agent_id: agent, event });
  return pins.some((p) => pinKey(p) === key);
}

/** Pins `pin` at the end, or unpins it when it is already pinned. */
export function togglePin(pins: Pin[], pin: Pin): Pin[] {
  const key = pinKey(pin);
  return pins.some((p) => pinKey(p) === key)
    ? pins.filter((p) => pinKey(p) !== key)
    : [...pins, pin];
}

/** Moves the pin `key` before (or after) the pin `target`, as a drop does. */
export function movePin(pins: Pin[], key: string, target: string, after: boolean): Pin[] {
  const moving = pins.find((p) => pinKey(p) === key);
  if (!moving || key === target) return pins;
  const rest = pins.filter((p) => pinKey(p) !== key);
  const at = rest.findIndex((p) => pinKey(p) === target);
  if (at < 0) return pins;
  rest.splice(after ? at + 1 : at, 0, moving);
  return rest;
}
