// An original little character drawn in SVG: body shape, eyes and color come
// from the agent's look; CSS animates blinking and the mood (see styles.css).
import type { JSX } from "preact";

import type { Character, Mood } from "../lib/character.ts";

/** Eye height per body, in the 48x48 view box. */
const EYE_Y: Record<Character["body"], number> = { blob: 29, bean: 24, pebble: 32, sprout: 32 };

function BodyShape(props: { body: Character["body"] }): JSX.Element {
  switch (props.body) {
    case "blob":
      return <path class="char-body" d="M7 52 C7 27 14 15 24 15 C34 15 41 27 41 52 Z" />;
    case "bean":
      return <rect class="char-body" x="11" y="9" width="26" height="46" rx="13" />;
    case "pebble":
      return <ellipse class="char-body" cx="24" cy="37" rx="20" ry="17" />;
    case "sprout":
      return (
        <>
          <path class="char-stem" d="M24 20 C24 15 25 12 27 10" />
          <ellipse
            class="char-leaf"
            cx="30.5"
            cy="9.5"
            rx="4.2"
            ry="2.3"
            transform="rotate(-28 30.5 9.5)"
          />
          <path class="char-body" d="M8 52 C8 30 15 19 24 19 C33 19 40 30 40 52 Z" />
        </>
      );
  }
}

function OpenEyes(props: { eyes: Character["eyes"]; y: number }): JSX.Element {
  const { y } = props;
  const xs = [17.5, 30.5];
  switch (props.eyes) {
    case "dots":
      return (
        <>
          {xs.map((x) => (
            <circle key={x} class="char-ink" cx={x} cy={y} r="2.7" />
          ))}
        </>
      );
    case "ovals":
      return (
        <>
          {xs.map((x) => (
            <g key={x}>
              <ellipse class="char-ink" cx={x} cy={y} rx="2.5" ry="3.7" />
              <circle class="char-shine" cx={x + 0.8} cy={y - 1.3} r="0.9" />
            </g>
          ))}
        </>
      );
    case "wide":
      return (
        <>
          {xs.map((x) => (
            <g key={x}>
              <circle class="char-white" cx={x} cy={y} r="4.6" />
              <circle class="char-ink char-pupil" cx={x} cy={y} r="2.3" />
            </g>
          ))}
        </>
      );
    case "visor":
      return (
        <>
          <rect class="char-ink" x="12" y={y - 4.2} width="24" height="8.4" rx="4.2" />
          {xs.map((x) => (
            <circle key={x} class="char-glow" cx={x} cy={y} r="1.8" />
          ))}
        </>
      );
  }
}

export function CharacterFace(props: {
  look: Character;
  mood: Mood;
  /** Seconds; staggers blinking between avatars. */
  blinkDelay: number;
}): JSX.Element {
  const { look, mood } = props;
  const y = EYE_Y[look.body];
  const style = { "--char-hue": String(look.hue), "--blink-delay": `${String(props.blinkDelay)}s` };
  return (
    <svg
      class={`char m-${mood} b-${look.body}`}
      viewBox="0 0 48 48"
      style={style}
      aria-hidden="true"
    >
      <rect class="char-bg" width="48" height="48" />
      <g class="char-figure">
        <BodyShape body={look.body} />
        <ellipse class="char-cheek" cx="13" cy={y + 6} rx="2.6" ry="1.5" />
        <ellipse class="char-cheek" cx="35" cy={y + 6} rx="2.6" ry="1.5" />
        {mood === "sleeping" ? (
          <g class="char-closed">
            {look.eyes === "visor" ? (
              <path d={`M14 ${String(y)} H34`} />
            ) : (
              <>
                <path d={`M14.5 ${String(y)} q3 2.6 6 0`} />
                <path d={`M27.5 ${String(y)} q3 2.6 6 0`} />
              </>
            )}
          </g>
        ) : (
          <g class="char-look">
            <g class="char-eyes">
              <OpenEyes eyes={look.eyes} y={y} />
            </g>
          </g>
        )}
      </g>
    </svg>
  );
}
