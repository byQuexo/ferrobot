import type { JSX } from "preact";
import { Paperclip } from "lucide-preact";

import type { Attachment } from "../lib/timeline.ts";

/** Object URLs of images sent from this window by `<agent>/<path>`, for their chips. */
// shortcut: the URLs live until the window closes and older images show no thumbnail; load them from the workspace if that matters.
export const previews = new Map<string, string>();

/** The files attached to an operator's message. */
export function AttachmentChips(props: { agent: string; files: Attachment[] }): JSX.Element {
  return (
    <div class="attachments">
      {props.files.map((f) => {
        const preview = previews.get(`${props.agent}/${f.path}`);
        return (
          <span key={f.path} class="attachment" title={`${f.path} (${f.kind}, ${f.size})`}>
            {preview ? <img src={preview} alt="" /> : <Paperclip size={14} />}
            <span>{f.path.split("/").pop()}</span>
          </span>
        );
      })}
    </div>
  );
}
