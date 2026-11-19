// Stable hashing for generated avatars, so every client derives the same
// look for the same agent.

/** FNV-1a 32-bit; stable across platforms and runs. */
export function hash(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h;
}
