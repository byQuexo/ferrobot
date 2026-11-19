// Generated avatars: initials on a color derived from the agent's name, so
// every client shows the same color for the same agent.

/** Up to two letters: "code-reviewer" -> "CR", "ada" -> "AD". */
export function initials(name: string): string {
  const parts = name.split(/[-_\s]+/).filter(Boolean);
  if (parts.length >= 2) return `${parts[0]?.[0] ?? ""}${parts[1]?.[0] ?? ""}`.toUpperCase();
  return name.slice(0, 2).toUpperCase();
}

/** FNV-1a 32-bit; stable across platforms and runs. */
export function hash(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h;
}

/** A hue from a curated set that reads well in light and dark themes. */
const HUES = [12, 28, 45, 95, 150, 172, 192, 212, 232, 262, 288, 320, 342];

export function avatarHue(name: string): number {
  return HUES[hash(name) % HUES.length] ?? 212;
}
