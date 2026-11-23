// Markdown for chat messages, rendered as elements: no HTML is ever
// interpreted in the app (```html previews run in a sandboxed frame), and links are http(s) only.
import type { JSX } from "preact";
import { FileText, Link } from "lucide-preact";

import { useApp } from "../context.ts";
import { linkLabel } from "../lib/format.ts";
import { splitWikiLinks } from "../lib/guides.ts";
import { parseBlocks, parseInline, type Block, type Inline } from "../lib/markdown.ts";
import { mentionsAsText, splitMentions } from "../lib/mentions.ts";
import { Avatar } from "./Avatar.tsx";
import { CodeBlock, isPreviewLang } from "./CodeBlock.tsx";

type Plain = (text: string) => string;
/** Opens a wiki page; set only for guides, so chat text never turns `[[x]]` into links. */
type OnPage = ((page: string) => void) | undefined;

/** Files shared in this chat by name, so a mentioned file name opens it. */
export interface ChatFiles {
  agent: string;
  byName: ReadonlyMap<string, number>;
}

export function RichText(props: {
  text: string;
  files?: ChatFiles;
  onPage?: (page: string) => void;
}): JSX.Element {
  const { agents } = useApp();
  // Code shows mention tokens as plain `@name`, not as chips.
  const plain: Plain = (t) => mentionsAsText(t, (id) => agents.get(id)?.name);
  return (
    <div class="rich">
      {parseBlocks(props.text).map((b, i) => (
        <BlockView key={i} block={b} plain={plain} files={props.files} onPage={props.onPage} />
      ))}
    </div>
  );
}

function BlockView(props: {
  block: Block;
  plain: Plain;
  files: ChatFiles | undefined;
  onPage: OnPage;
}): JSX.Element {
  const { block, plain } = props;
  const inline = (text: string): JSX.Element => (
    <Inlines nodes={parseInline(text)} plain={plain} files={props.files} onPage={props.onPage} />
  );
  switch (block.kind) {
    case "heading": {
      const Tag = (["h3", "h4", "h5"] as const)[block.level - 1] ?? "h5";
      return <Tag>{inline(block.text)}</Tag>;
    }
    case "para":
      return <p>{inline(block.text)}</p>;
    case "list": {
      const items = block.items.map((item, i) => (
        <li key={i} style={item.depth ? { marginLeft: `${String(item.depth * 16)}px` } : undefined}>
          {inline(item.text)}
        </li>
      ));
      return block.ordered ? <ol start={block.start}>{items}</ol> : <ul>{items}</ul>;
    }
    case "quote":
      return (
        <blockquote>
          <p>{inline(block.text)}</p>
        </blockquote>
      );
    case "code":
      // A block still streaming shows as code, so its preview does not redraw on every word.
      if (isPreviewLang(block.lang) && !block.open)
        return <CodeBlock lang={block.lang} code={plain(block.text)} />;
      return (
        <pre class="code-block">
          <code>{plain(block.text)}</code>
        </pre>
      );
    case "hr":
      return <hr />;
    case "table":
      return (
        <div class="md-table">
          <table>
            <thead>
              <tr>
                {block.head.map((c, i) => (
                  <th key={i}>{inline(c)}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {block.rows.map((row, r) => (
                <tr key={r}>
                  {row.map((c, i) => (
                    <td key={i}>{inline(c)}</td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
  }
}

function Inlines(props: {
  nodes: Inline[];
  plain: Plain;
  files?: ChatFiles | undefined;
  onPage?: OnPage;
}): JSX.Element {
  const { plain, files, onPage } = props;
  const { openFile } = useApp();
  return (
    <>
      {props.nodes.map((n, i) => {
        if ("text" in n) {
          if (!onPage) return n.text;
          return splitWikiLinks(n.text).map((part, j) =>
            "text" in part ? (
              part.text
            ) : (
              <button
                key={`${String(i)}-${String(j)}`}
                type="button"
                class="text-link"
                onClick={() => {
                  onPage(part.page);
                }}
              >
                {part.label}
              </button>
            ),
          );
        }
        if ("mention" in n) return <Mention key={i} id={n.mention} />;
        if ("strong" in n)
          return (
            <strong key={i}>
              <Inlines nodes={n.strong} plain={plain} files={files} onPage={onPage} />
            </strong>
          );
        if ("em" in n)
          return (
            <em key={i}>
              <Inlines nodes={n.em} plain={plain} files={files} onPage={onPage} />
            </em>
          );
        if ("link" in n) {
          const [only, ...others] = n.children;
          if (only && !others.length && "text" in only && only.text === n.link) {
            return (
              <a
                key={i}
                class="link-chip"
                href={n.link}
                target="_blank"
                rel="noreferrer noopener"
                title={n.link}
                aria-label={n.link}
              >
                <Link size={12} />
                <span>{linkLabel(n.link)}</span>
              </a>
            );
          }
          return (
            <a key={i} href={n.link} target="_blank" rel="noreferrer noopener">
              <Inlines nodes={n.children} plain={plain} />
            </a>
          );
        }
        const shared = files?.byName.get(n.code.split("/").pop() ?? n.code);
        if (files && shared !== undefined) {
          return (
            <button
              key={i}
              type="button"
              class="file-link"
              title={`Open ${n.code}`}
              onClick={() => {
                openFile(files.agent, shared);
              }}
            >
              <FileText size={12} />
              {n.code}
            </button>
          );
        }
        // A lone mention in backticks is still a mention.
        const parts = splitMentions(n.code);
        if (parts.length === 1 && parts[0] && "mention" in parts[0])
          return <Mention key={i} id={parts[0].mention} />;
        return <code key={i}>{plain(n.code)}</code>;
      })}
    </>
  );
}

/** An agent mention: current name and avatar; opens that agent's chat. */
function Mention(props: { id: string }): JSX.Element {
  const { agents, openAgent } = useApp();
  const agent = agents.get(props.id);
  if (!agent) return <span class="mention-gone">@unknown-agent</span>;
  return (
    <button
      type="button"
      class="mention"
      title={`Open ${agent.name}`}
      onClick={() => {
        openAgent(agent.id);
      }}
    >
      <Avatar agent={agent} size={16} />
      {agent.name}
    </button>
  );
}
