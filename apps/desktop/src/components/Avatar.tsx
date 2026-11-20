import type { JSX } from "preact";

import type { Agent, Presence } from "../api.ts";
import { useApp } from "../context.ts";
import { blinkDelay, characterOf, moodOf, type Character } from "../lib/character.ts";
import { CharacterFace } from "./Character.tsx";

export function Avatar(props: {
  agent: Pick<Agent, "name" | "avatar_url"> & { id?: string; character?: Character | null };
  size?: number;
  presence?: Presence | undefined;
  /** Local preview (data/blob URL) that overrides the stored avatar. */
  preview?: string | null;
  /** Animate the character's mood for this presence without the status dot. */
  mood?: Presence | undefined;
}): JSX.Element {
  const { client } = useApp();
  const size = props.size ?? 36;
  const src = props.preview ?? (props.agent.avatar_url ? client.url(props.agent.avatar_url) : null);
  const seed = props.agent.id ?? props.agent.name;
  const style = { width: `${String(size)}px`, height: `${String(size)}px` };
  return (
    <span class={`avatar${src ? "" : " avatar-char"}`} style={style}>
      {src ? (
        <img src={src} alt="" draggable={false} />
      ) : (
        <CharacterFace
          look={characterOf(props.agent)}
          mood={moodOf(props.mood ?? props.presence)}
          blinkDelay={blinkDelay(seed)}
        />
      )}
      {props.presence && <span class={`presence-dot p-${props.presence}`} aria-hidden="true" />}
    </span>
  );
}
