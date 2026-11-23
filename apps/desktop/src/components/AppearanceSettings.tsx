import type { ComponentChildren, JSX } from "preact";

import { ACCENTS, CHOICES, type Appearance } from "../lib/appearance.ts";

/** A settings row: title and hint on the left, its control on the right. */
export function PrefRow(props: {
  title: string;
  hint?: ComponentChildren;
  children: ComponentChildren;
}): JSX.Element {
  return (
    <li class="pref-row">
      <span class="pref-text">
        <span class="pref-title">{props.title}</span>
        {props.hint && <span class="hint">{props.hint}</span>}
      </span>
      {props.children}
    </li>
  );
}

/** Theme, accent, bubbles, chat background and text size; saved on this computer. */
export function AppearanceSettings(props: {
  look: Appearance;
  onChange: (look: Appearance) => void;
}): JSX.Element {
  const set = <K extends keyof Appearance>(k: K, v: Appearance[K]): void => {
    props.onChange({ ...props.look, [k]: v });
  };
  const segments = (k: Exclude<keyof Appearance, "accent">, label: string): JSX.Element => (
    <div class="segmented" role="radiogroup" aria-label={label}>
      {CHOICES[k].map(([v, text]) => (
        <button
          key={v}
          type="button"
          role="radio"
          aria-checked={props.look[k] === v}
          onClick={() => {
            set(k, v);
          }}
        >
          {text}
        </button>
      ))}
    </div>
  );
  return (
    <ul class="pref-list">
      <PrefRow title="Theme" hint="Light, dark, or the same as your system.">
        {segments("theme", "Theme")}
      </PrefRow>
      <PrefRow title="Accent color" hint="Buttons, links and highlights.">
        <div class="swatches" role="radiogroup" aria-label="Accent color">
          {CHOICES.accent.map(([id, name]) => (
            <button
              key={id}
              type="button"
              role="radio"
              class="swatch"
              aria-checked={props.look.accent === id}
              aria-label={name}
              title={name}
              style={{ "--swatch-l": ACCENTS[id].light[0], "--swatch-d": ACCENTS[id].dark[0] }}
              onClick={() => {
                set("accent", id);
              }}
            />
          ))}
        </div>
      </PrefRow>
      <PrefRow title="Your bubbles" hint="How your own messages look.">
        {segments("bubbles", "Your bubbles")}
      </PrefRow>
      <PrefRow title="Chat background" hint="Behind the messages.">
        {segments("chatBg", "Chat background")}
      </PrefRow>
      <PrefRow title="Text size" hint="Messages and the message box.">
        {segments("textSize", "Text size")}
      </PrefRow>
    </ul>
  );
}
