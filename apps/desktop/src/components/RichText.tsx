// Minimal, safe text rendering: fenced code blocks and inline `code`.
// Everything else is plain text (no HTML is ever interpreted).
import type { JSX } from "preact";

export function RichText(props: { text: string }): JSX.Element {
  const parts: JSX.Element[] = [];
  const fence = /```[\w-]*\n?([\s\S]*?)```/g;
  let last = 0;
  let i = 0;
  for (const m of props.text.matchAll(fence)) {
    if (m.index > last) parts.push(<Inline key={i++} text={props.text.slice(last, m.index)} />);
    parts.push(<pre key={i++} class="code-block"><code>{m[1]?.replace(/\n$/, "")}</code></pre>);
    last = m.index + m[0].length;
  }
  if (last < props.text.length) parts.push(<Inline key={i++} text={props.text.slice(last)} />);
  return <div class="rich">{parts}</div>;
}

function Inline(props: { text: string }): JSX.Element {
  const text = props.text.replace(/^\n+|\n+$/g, "");
  if (!text) return <></>;
  const bits = text.split(/(`[^`\n]+`)/g);
  return (
    <p>
      {bits.map((b, i) =>
        b.length > 2 && b.startsWith("`") && b.endsWith("`") ? <code key={i}>{b.slice(1, -1)}</code> : b,
      )}
    </p>
  );
}
