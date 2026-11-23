// Mention tokens: the daemon stores `@name` mentions as `<@agent-id>` so
// they survive renames. The UI only splits them out and renders current names.

export type TextPart = { text: string } | { mention: string };

const TOKEN = /<@([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})>/g;

export function splitMentions(text: string): TextPart[] {
  const out: TextPart[] = [];
  let last = 0;
  for (const m of text.matchAll(TOKEN)) {
    if (m.index > last) out.push({ text: text.slice(last, m.index) });
    out.push({ mention: m[1] ?? "" });
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push({ text: text.slice(last) });
  return out;
}

/** Plain-text version with current names (for previews). */
export function mentionsAsText(text: string, nameOf: (id: string) => string | undefined): string {
  return splitMentions(text)
    .map((p) => ("text" in p ? p.text : `@${nameOf(p.mention) ?? "unknown-agent"}`))
    .join("");
}

/** The `@name` being typed just before `caret`: `query` (lower case) and `at`, the `@`'s index. */
export function mentionQuery(text: string, caret: number): { query: string; at: number } | null {
  const m = /(?:^|\s)@([a-z0-9-]*)$/i.exec(text.slice(0, caret));
  if (!m) return null;
  const query = m[1] ?? "";
  return { query: query.toLowerCase(), at: caret - query.length - 1 };
}
