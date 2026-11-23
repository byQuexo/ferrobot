import type { JSX } from "preact";
import { useState } from "preact/hooks";
import { ArrowDownLeft, ArrowUpRight, ChevronRight } from "lucide-preact";

import { useApp } from "../context.ts";
import { clockTime } from "../lib/format.ts";
import { exchangeRows, type Exchange, type ExchangeRow } from "../lib/timeline.ts";
import { Avatar } from "./Avatar.tsx";
import { RichText } from "./RichText.tsx";

/** Up to three overlapping avatars of the agents among `ids`. */
function Faces(props: { ids: string[] }): JSX.Element {
  const { agents } = useApp();
  return (
    <span class="pair-avatars">
      {props.ids
        .flatMap((id) => agents.get(id) ?? [])
        .slice(0, 3)
        .map((a) => (
          <Avatar key={a.id} agent={a} size={18} />
        ))}
    </span>
  );
}

/** "N messages with K agents": a run of mail with several agents, one row per message when open. */
export function MailGroup(props: { item: Exchange }): JSX.Element {
  const [open, setOpen] = useState(false);
  const rows = exchangeRows(props.item.messages);
  return (
    <>
      <div class={`row row-center exchange${open ? " open" : ""}`}>
        <button
          type="button"
          class="exchange-hint"
          aria-expanded={open}
          onClick={() => {
            setOpen(!open);
          }}
        >
          <span>
            {rows.length} {rows.length === 1 ? "message" : "messages"} with
          </span>
          <Faces ids={props.item.peers} />
          <strong>{props.item.peers.length} agents</strong>
          <ChevronRight size={16} class="chev" />
        </button>
      </div>
      {open && rows.map((r) => <MailRow key={r.key} row={r} />)}
    </>
  );
}

function MailRow(props: { row: ExchangeRow }): JSX.Element {
  const { row } = props;
  const { agents } = useApp();
  const [open, setOpen] = useState(false);
  const names = row.peers.map((p) => agents.get(p.id)?.name ?? p.name);
  return (
    <div class={`row row-a2a ${row.type}`}>
      <div class={`a2a mail-row${open ? " open" : ""}`}>
        <button
          type="button"
          class="a2a-head mail-row-head"
          aria-expanded={open}
          title={names.join(", ")}
          onClick={() => {
            setOpen(!open);
          }}
        >
          <ChevronRight size={14} class="chev" />
          {row.type === "incoming" ? <ArrowDownLeft size={16} /> : <ArrowUpRight size={16} />}
          <Faces ids={row.peers.map((p) => p.id)} />
          <span>
            {row.type === "incoming"
              ? "Message from "
              : names.length > 1
                ? "Messaged "
                : "Message to "}
            <strong>{names.length > 1 ? `${String(names.length)} agents` : names[0]}</strong>
          </span>
          <span class="spacer" />
          <span class="meta">{clockTime(row.at)}</span>
        </button>
        {open && <RichText text={row.text} />}
      </div>
    </div>
  );
}
