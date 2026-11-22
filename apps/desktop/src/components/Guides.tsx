import type { JSX } from "preact";
import { useEffect, useRef } from "preact/hooks";
import { ArrowLeft } from "lucide-preact";

import { GUIDES } from "../lib/guides.ts";
import { RichText } from "./RichText.tsx";

// The GitHub wiki's pages, bundled at build time so the app and the wiki share one source.
const PAGES: Record<string, string> = Object.fromEntries(
  Object.entries(
    import.meta.glob<string>("../../../../docs/wiki/*.md", {
      query: "?raw",
      import: "default",
      eager: true,
    }),
  ).map(([path, text]) => [path.slice(path.lastIndexOf("/") + 1, -".md".length), text]),
);

/** The guide list, or one guide; wiki links open other guides in place. */
export function Guides(props: {
  page: string | null;
  onPage: (page: string | null) => void;
}): JSX.Element {
  const ref = useRef<HTMLDivElement>(null);
  const text = props.page ? PAGES[props.page] : undefined;
  useEffect(() => {
    ref.current?.closest(".settings-body")?.scrollTo(0, 0);
  }, [props.page]);
  const title = GUIDES.find((g) => g.page === props.page)?.title ?? props.page?.replace(/-/g, " ");

  return (
    <div ref={ref}>
      {props.page && text !== undefined ? (
        <article class="guide">
          <button
            type="button"
            class="text-link guide-back"
            onClick={() => {
              props.onPage(null);
            }}
          >
            <ArrowLeft size={14} /> All guides
          </button>
          <h3 class="guide-title">{title}</h3>
          <RichText text={text} onPage={props.onPage} />
        </article>
      ) : (
        <ul class="guide-list">
          {GUIDES.filter((g) => g.page in PAGES).map((g) => (
            <li key={g.page}>
              <button
                type="button"
                class="guide-card"
                onClick={() => {
                  props.onPage(g.page);
                }}
              >
                <b>{g.title}</b>
                <span class="muted small">{g.about}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
