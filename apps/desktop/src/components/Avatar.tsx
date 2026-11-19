import type { JSX } from "preact";

import type { Agent, Presence } from "../api.ts";
import { useApp } from "../context.ts";
import { avatarHue, initials } from "../lib/avatar.ts";

export function Avatar(props: {
  agent: Pick<Agent, "name" | "avatar_url">;
  size?: number;
  presence?: Presence | undefined;
  /** Local preview (data/blob URL) that overrides the stored avatar. */
  preview?: string | null;
}): JSX.Element {
  const { client } = useApp();
  const size = props.size ?? 36;
  const src = props.preview ?? (props.agent.avatar_url ? client.url(props.agent.avatar_url) : null);
  const style = {
    width: `${String(size)}px`,
    height: `${String(size)}px`,
    fontSize: `${String(Math.round(size * 0.38))}px`,
    "--hue": String(avatarHue(props.agent.name)),
  };
  return (
    <span class="avatar" style={style}>
      {src ? <img src={src} alt="" draggable={false} /> : <span class="avatar-initials">{initials(props.agent.name)}</span>}
      {props.presence && <span class={`presence-dot p-${props.presence}`} aria-hidden="true" />}
    </span>
  );
}
