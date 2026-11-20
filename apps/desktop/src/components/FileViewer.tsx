import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";

import type { SharedFile } from "../api.ts";
import { useApp } from "../context.ts";
import { RichText } from "./RichText.tsx";
import { Button, Dialog, Skeleton } from "./ui.tsx";

export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${String(bytes)} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** A file an agent shared: Markdown and text inline, .html as a sandboxed page. */
export function FileViewer(props: { agent: string; file: number; onClose: () => void }): JSX.Element {
  const { client, toast } = useApp();
  const [file, setFile] = useState<SharedFile | null>(null);

  useEffect(() => {
    client.sharedFile(props.agent, props.file).then(setFile, (err: unknown) => {
      toast(err instanceof Error ? err.message : String(err));
      props.onClose();
    });
  }, [props.agent, props.file]);

  const close = <Button onClick={props.onClose}>Close</Button>;
  if (!file) return <Dialog title="Opening…" onClose={props.onClose} footer={close} wide><Skeleton lines={8} /></Dialog>;
  let body: JSX.Element;
  if (file.page_url) {
    // No allow-same-origin: the page cannot reach the app, its storage or the token.
    body = <iframe class="file-frame" title={file.name} sandbox="allow-scripts" src={`${client.base}${file.page_url}`} />;
  } else if (file.content === null || file.content === undefined) {
    body = <p class="muted">No preview for this file ({fileSize(file.size)}).</p>;
  } else if (/\.(md|markdown)$/i.test(file.name)) {
    body = <RichText text={file.content} />;
  } else {
    body = <pre class="code-block"><code>{file.content}</code></pre>;
  }
  return (
    <Dialog title={file.name} onClose={props.onClose} footer={close} wide>
      <div class="file-view">{body}</div>
    </Dialog>
  );
}
