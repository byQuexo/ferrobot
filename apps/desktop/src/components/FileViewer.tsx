import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Download } from "lucide-preact";

import type { SharedFile } from "../api.ts";
import { useApp } from "../context.ts";
import { RichText } from "./RichText.tsx";
import { Button, Dialog, Skeleton } from "./ui.tsx";

export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${String(bytes)} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/**
 * A file an agent shared: .html opens full screen in the browser, a PDF in a
 * full-screen viewer, Markdown and text inline. Every file can be
 * downloaded (the browser saves it; the app window cannot).
 */
export function FileViewer(props: { agent: string; file: number; onClose: () => void }): JSX.Element | null {
  const { client, toast } = useApp();
  const [file, setFile] = useState<SharedFile | null>(null);
  const open = (path: string): void => { window.open(`${client.base}${path}`, "_blank", "noopener"); };

  useEffect(() => {
    client.sharedFile(props.agent, props.file).then(
      (f) => {
        if (f.page_url) {
          open(f.page_url);
          props.onClose();
        } else {
          setFile(f);
        }
      },
      (err: unknown) => {
        toast(err instanceof Error ? err.message : String(err));
        props.onClose();
      },
    );
  }, [props.agent, props.file]);

  if (!file) {
    return <Dialog title="Opening…" onClose={props.onClose} wide><Skeleton lines={8} /></Dialog>;
  }
  const footer = (
    <>
      <Button onClick={() => { open(`${file.url}?download=1`); }}>
        <Download size={15} /> Download
      </Button>
      <Button onClick={props.onClose}>Close</Button>
    </>
  );
  if (/\.pdf$/i.test(file.name) && file.content == null) {
    return (
      <Dialog title={file.name} onClose={props.onClose} footer={footer} full>
        <iframe class="file-pdf" title={file.name} src={`${client.base}${file.url}`} />
      </Dialog>
    );
  }
  let body: JSX.Element;
  if (file.content == null) {
    body = <p class="muted">No preview for this file ({fileSize(file.size)}). Download it to open it.</p>;
  } else if (/\.(md|markdown)$/i.test(file.name)) {
    body = <RichText text={file.content} />;
  } else {
    body = <pre class="code-block"><code>{file.content}</code></pre>;
  }
  return (
    <Dialog title={file.name} onClose={props.onClose} footer={footer} wide>
      <div class="file-view">{body}</div>
    </Dialog>
  );
}
