// A small Markdown parser for chat messages. It returns plain data that the
// UI turns into elements, so no HTML string is ever interpreted.

export type Inline =
  | { text: string }
  | { code: string }
  | { strong: Inline[] }
  | { em: Inline[] }
  | { link: string; children: Inline[] }
  | { mention: string };

export type Block =
  | { kind: "heading"; level: number; text: string }
  | { kind: "para"; text: string }
  | {
      kind: "list";
      ordered: boolean;
      /** First number of an ordered list that does not start at 1. */
      start?: number;
      items: { text: string; depth: number }[];
    }
  | { kind: "quote"; text: string }
  /** `lang`: the fence's info word, lowercase; `open`: no closing fence yet (still streaming). */
  | { kind: "code"; text: string; lang?: string; open?: true }
  | { kind: "hr" }
  | { kind: "table"; head: string[]; rows: string[][] };

const ITEM = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;
const ROW = /^\s*\|.*\|\s*$/;
const RULE = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/;

const cells = (line: string): string[] =>
  line
    .trim()
    .replace(/^\||\|$/g, "")
    .split("|")
    .map((c) => c.trim());

export function parseBlocks(src: string): Block[] {
  const lines = src.replace(/\r\n?/g, "\n").split("\n");
  const out: Block[] = [];
  let para: string[] = [];
  const flush = (): void => {
    if (para.length) out.push({ kind: "para", text: para.join("\n") });
    para = [];
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i] ?? "";
    const fence = /^\s*```\s*([\w+-]*)/.exec(line);
    if (fence) {
      flush();
      const code: string[] = [];
      // A fence indented in a list item indents its code lines as far.
      const indent = new RegExp(`^ {0,${String(line.length - line.trimStart().length)}}`);
      while (++i < lines.length && !/^\s*```/.test(lines[i] ?? ""))
        code.push((lines[i] ?? "").replace(indent, ""));
      const lang = fence[1]?.toLowerCase();
      out.push({
        kind: "code",
        text: code.join("\n"),
        ...(lang ? { lang } : {}),
        ...(i >= lines.length ? { open: true as const } : {}),
      });
      continue;
    }
    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      flush();
      out.push({
        kind: "heading",
        level: Math.min(heading[1]?.length ?? 1, 3),
        text: heading[2] ?? "",
      });
      continue;
    }
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) {
      flush();
      out.push({ kind: "hr" });
      continue;
    }
    if (ROW.test(line) && RULE.test(lines[i + 1] ?? "")) {
      flush();
      const head = cells(line);
      const rows: string[][] = [];
      for (i += 2; i < lines.length && ROW.test(lines[i] ?? ""); i++)
        rows.push(cells(lines[i] ?? ""));
      i--;
      out.push({ kind: "table", head, rows });
      continue;
    }
    const item = ITEM.exec(line);
    if (item) {
      flush();
      const ordered = /\d/.test(item[2] ?? "");
      const last = out[out.length - 1];
      const entry = { text: item[3] ?? "", depth: Math.floor((item[1]?.length ?? 0) / 2) };
      if (last?.kind === "list" && last.ordered === ordered) last.items.push(entry);
      else {
        const start = Number.parseInt(item[2] ?? "", 10);
        out.push({ kind: "list", ordered, ...(start > 1 ? { start } : {}), items: [entry] });
      }
      continue;
    }
    // An indented line right after a list item continues that item.
    const prev = out[out.length - 1];
    if (!para.length && prev?.kind === "list" && /^\s+\S/.test(line) && lines[i - 1]?.trim()) {
      const lastItem = prev.items[prev.items.length - 1];
      if (lastItem) lastItem.text += ` ${line.trim()}`;
      continue;
    }
    const quote = /^\s*>\s?(.*)$/.exec(line);
    if (quote) {
      flush();
      const last = out[out.length - 1];
      if (last?.kind === "quote") last.text += `\n${quote[1] ?? ""}`;
      else out.push({ kind: "quote", text: quote[1] ?? "" });
      continue;
    }
    if (!line.trim()) {
      flush();
      continue;
    }
    para.push(line);
  }
  flush();
  return out;
}

// Code first so nothing inside it is parsed; links and bare URLs only http(s).
const INLINE = new RegExp(
  [
    "`([^`\\n]+)`",
    "\\*\\*(.+?)\\*\\*",
    "\\[([^\\]\\n]+)\\]\\((https?:\\/\\/[^)\\s]+)\\)",
    "<@([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})>",
    "(https?:\\/\\/[^\\s<>()]*[^\\s<>().,;:!?'\"])",
    "(?<![\\w*])\\*([^*\\n]+?)\\*(?![\\w*])",
    "(?<!\\w)_([^_\\n]+?)_(?!\\w)",
  ].join("|"),
  "g",
);

export function parseInline(src: string): Inline[] {
  const out: Inline[] = [];
  let last = 0;
  for (const m of src.matchAll(INLINE)) {
    if (m.index > last) out.push({ text: src.slice(last, m.index) });
    const [, code, strong, label, href, mention, url, em1, em2] = m;
    if (code !== undefined) out.push({ code });
    else if (strong !== undefined) out.push({ strong: parseInline(strong) });
    else if (label !== undefined && href !== undefined)
      out.push({ link: href, children: parseInline(label) });
    else if (mention !== undefined) out.push({ mention });
    else if (url !== undefined) out.push({ link: url, children: [{ text: url }] });
    else out.push({ em: parseInline(em1 ?? em2 ?? "") });
    last = m.index + m[0].length;
  }
  if (last < src.length) out.push({ text: src.slice(last) });
  return out;
}
