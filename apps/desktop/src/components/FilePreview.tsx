import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Maximize2 } from "lucide-preact";

import { useApp } from "../context.ts";

const kindOf = (name: string): "page" | "pdf" | null =>
  /\.html?$/i.test(name) ? "page" : /\.pdf$/i.test(name) ? "pdf" : null;

/** A shared page or PDF, shown under its file card; Expand opens the file viewer. */
export function FilePreview(props: {
  agent: string;
  file: number;
  name: string;
}): JSX.Element | null {
  const { client, openFile } = useApp();
  const kind = kindOf(props.name);
  const [src, setSrc] = useState<string | null>(null);
  const [none, setNone] = useState(false);
  useEffect(() => {
    if (!kind) return;
    let live = true;
    client.sharedFile(props.agent, props.file).then(
      (f) => {
        if (!live) return;
        // The same rule as the viewer: a ".pdf" that is text shows as text there, not here.
        const path = kind === "page" ? f.page_url : f.content == null ? f.url : null;
        if (path) setSrc(`${client.base}${path}`);
        else setNone(true);
      },
      () => {
        if (live) setNone(true);
      },
    );
    return () => {
      live = false;
    };
  }, [props.agent, props.file]);
  if (!kind || none) return null;
  return (
    <div class="block file-preview">
      <div class="block-head">
        <span class="block-label">{kind === "page" ? "Page" : "PDF"}</span>
        <span class="spacer" />
        <button
          type="button"
          class="block-btn"
          title="Expand"
          aria-label="Expand"
          onClick={() => {
            openFile(props.agent, props.file);
          }}
        >
          <Maximize2 size={14} />
        </button>
      </div>
      {/* The box keeps its height while loading, so the chat's scroll position holds. */}
      <div class="block-body file-preview-body">
        {src &&
          (kind === "page" ? (
            <iframe title={props.name} sandbox="allow-scripts" src={src} />
          ) : (
            // A sandbox would stop the PDF viewer; the daemon serves PDFs with a CSP of their own.
            <iframe title={props.name} src={src} />
          ))}
      </div>
    </div>
  );
}
