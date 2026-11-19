// Minimal, safe text rendering: fenced code blocks, inline `code` and
// agent mentions. Everything else is plain text (no HTML is ever interpreted).
import type { JSX } from "preact";

import { useApp } from "../context.ts";
import { splitMentions } from "../lib/mentions.ts";
import { Avatar } from "./Avatar.tsx";

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
        b.length > 2 && b.startsWith("`") && b.endsWith("`") ? (
          <code key={i}>{b.slice(1, -1)}</code>
        ) : (
          splitMentions(b).map((p, j) => ("text" in p ? p.text : <Mention key={`${String(i)}-${String(j)}`} id={p.mention} />))
        ),
      )}
    </p>
  );
}

/** An agent mention: current name and avatar; opens that agent's chat. */
function Mention(props: { id: string }): JSX.Element {
  const { agents, openAgent } = useApp();
  const agent = agents.get(props.id);
  if (!agent) return <span class="mention-gone">@unknown-agent</span>;
  return (
    <button type="button" class="mention" title={`Open ${agent.name}`} onClick={() => { openAgent(agent.id); }}>
      <Avatar agent={agent} size={16} />
      {agent.name}
    </button>
  );
}
