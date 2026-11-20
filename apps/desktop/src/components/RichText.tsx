// Minimal, safe text rendering: fenced code blocks, inline `code` and
// agent mentions. Everything else is plain text (no HTML is ever interpreted).
import type { JSX } from "preact";

import { useApp } from "../context.ts";
import { mentionsAsText, splitMentions } from "../lib/mentions.ts";
import { Avatar } from "./Avatar.tsx";

export function RichText(props: { text: string }): JSX.Element {
  const { agents } = useApp();
  // Code shows mention tokens as plain `@name`, not as chips.
  const plain = (t: string): string => mentionsAsText(t, (id) => agents.get(id)?.name);
  const parts: JSX.Element[] = [];
  const fence = /```[\w-]*\n?([\s\S]*?)```/g;
  let last = 0;
  let i = 0;
  for (const m of props.text.matchAll(fence)) {
    if (m.index > last) parts.push(<Inline key={i++} text={props.text.slice(last, m.index)} plain={plain} />);
    parts.push(<pre key={i++} class="code-block"><code>{plain(m[1]?.replace(/\n$/, "") ?? "")}</code></pre>);
    last = m.index + m[0].length;
  }
  if (last < props.text.length) parts.push(<Inline key={i++} text={props.text.slice(last)} plain={plain} />);
  return <div class="rich">{parts}</div>;
}

function Inline(props: { text: string; plain: (t: string) => string }): JSX.Element {
  const text = props.text.replace(/^\n+|\n+$/g, "");
  if (!text) return <></>;
  const bits = text.split(/(`[^`\n]+`)/g);
  return (
    <p>
      {bits.map((b, i) => {
        const code = b.length > 2 && b.startsWith("`") && b.endsWith("`");
        const parts = splitMentions(code ? b.slice(1, -1) : b);
        // A lone mention in backticks is still a mention.
        if (code && !(parts.length === 1 && parts[0] && "mention" in parts[0])) {
          return <code key={i}>{props.plain(b.slice(1, -1))}</code>;
        }
        return parts.map((p, j) => ("text" in p ? p.text : <Mention key={`${String(i)}-${String(j)}`} id={p.mention} />));
      })}
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
