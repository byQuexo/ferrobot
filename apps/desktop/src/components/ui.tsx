import type { ComponentChildren, JSX, RefObject } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { X } from "lucide-preact";

export function IconButton(props: {
  label: string;
  onClick: () => void;
  children: ComponentChildren;
  disabled?: boolean;
  tone?: "default" | "danger" | "accent";
  pressed?: boolean;
  expanded?: boolean;
}): JSX.Element {
  return (
    <button
      type="button"
      class={`icon-btn tone-${props.tone ?? "default"}`}
      aria-label={props.label}
      title={props.label}
      aria-pressed={props.pressed}
      aria-expanded={props.expanded}
      disabled={props.disabled}
      onClick={props.onClick}
    >
      {props.children}
    </button>
  );
}

export function Button(props: {
  onClick?: () => void;
  children: ComponentChildren;
  variant?: "primary" | "secondary" | "ghost" | "danger";
  type?: "button" | "submit";
  disabled?: boolean;
  small?: boolean;
}): JSX.Element {
  return (
    <button
      type={props.type ?? "button"}
      class={`btn btn-${props.variant ?? "secondary"}${props.small ? " btn-sm" : ""}`}
      disabled={props.disabled}
      onClick={props.onClick}
    >
      {props.children}
    </button>
  );
}

const CUSTOM = "\u0000custom";

/** A select with a "Custom…" entry that turns it into a text field. Without options it is the text field. */
export function Choice(props: {
  value: string;
  options: [string, string][];
  placeholder: string;
  onChange: (v: string) => void;
  label?: string;
  /** The text field reports its value on blur or Enter only, e.g. where each change is saved. */
  lazy?: boolean;
}): JSX.Element {
  const [custom, setCustom] = useState(false);
  const [draft, setDraft] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (custom) input.current?.focus();
  }, [custom]);
  if (custom || props.options.length === 0) {
    return (
      <span class="choice-custom">
        <input
          ref={input}
          value={draft ?? props.value}
          maxLength={200}
          spellcheck={false}
          autoComplete="off"
          aria-label={props.label}
          placeholder={props.placeholder}
          onInput={(e) => {
            if (props.lazy) setDraft(e.currentTarget.value);
            else props.onChange(e.currentTarget.value);
          }}
          onBlur={() => {
            if (draft !== null && draft !== props.value) props.onChange(draft);
            setDraft(null);
          }}
          onKeyDown={(e) => {
            if (props.lazy && e.key === "Enter") e.currentTarget.blur();
          }}
        />
        {props.options.length > 0 && (
          <Button
            small
            variant="ghost"
            onClick={() => {
              setCustom(false);
            }}
          >
            List
          </Button>
        )}
      </span>
    );
  }
  return (
    <select
      value={props.value}
      aria-label={props.label}
      onChange={(e) => {
        const v = e.currentTarget.value;
        if (v === CUSTOM) setCustom(true);
        else props.onChange(v);
      }}
    >
      <option value="">{props.placeholder}</option>
      {props.options.map(([v, label]) => (
        <option key={v} value={v}>
          {label}
        </option>
      ))}
      <option value={CUSTOM}>Custom…</option>
    </select>
  );
}

export function Switch(props: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
}): JSX.Element {
  return (
    <button
      type="button"
      role="switch"
      class="switch"
      aria-checked={props.checked}
      aria-label={props.label}
      title={props.label}
      onClick={() => {
        props.onChange(!props.checked);
      }}
    >
      <span class="switch-knob" />
    </button>
  );
}

/** A range slider in the accent color. `text` is what screen readers say instead of the number. */
export function Slider(props: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  text?: string;
  onInput: (v: number) => void;
  /** Once the slider is let go (or per arrow key). */
  onChange?: (v: number) => void;
}): JSX.Element {
  const { onChange } = props;
  return (
    <input
      type="range"
      class="slider"
      aria-label={props.label}
      aria-valuetext={props.text}
      min={props.min}
      max={props.max}
      step={props.step}
      value={props.value}
      style={{
        "--fill": `${String(((props.value - props.min) / (props.max - props.min)) * 100)}%`,
      }}
      onInput={(e) => {
        props.onInput(Number(e.currentTarget.value));
      }}
      onChange={
        onChange &&
        ((e) => {
          onChange(Number(e.currentTarget.value));
        })
      }
    />
  );
}

/** Closes a popover on Escape (`escape` true), or when a click or focus lands outside `ref`. */
export function useDismiss(
  ref: RefObject<HTMLElement>,
  open: boolean,
  onClose: (escape: boolean) => void,
): void {
  useEffect(() => {
    if (!open) return;
    // Focus moving out catches what a click does not, such as a dialog opening from the keyboard.
    const outside = (e: Event): void => {
      if (!ref.current?.contains(e.target as Node)) onClose(false);
    };
    const key = (e: KeyboardEvent): void => {
      if (e.key === "Escape") onClose(true);
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("focusin", outside);
    document.addEventListener("keydown", key);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("focusin", outside);
      document.removeEventListener("keydown", key);
    };
  }, [open]);
}

export function Dialog(props: {
  title: string;
  onClose: () => void;
  children: ComponentChildren;
  footer?: ComponentChildren;
  wide?: boolean;
  /** Fills the window (e.g. a PDF). */
  full?: boolean;
}): JSX.Element {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") props.onClose();
    };
    document.addEventListener("keydown", onKey);
    ref.current
      ?.querySelector<HTMLElement>("input:not([disabled]):not([hidden]), textarea")
      ?.focus();
    return () => {
      document.removeEventListener("keydown", onKey);
    };
  }, []);
  return (
    <div
      class="overlay"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) props.onClose();
      }}
    >
      <div
        class={`dialog${props.wide ? " dialog-wide" : ""}${props.full ? " dialog-full" : ""}`}
        role="dialog"
        aria-modal="true"
        aria-label={props.title}
        ref={ref}
      >
        <header class="dialog-head">
          <h2>{props.title}</h2>
          <IconButton label="Close" onClick={props.onClose}>
            <X size={18} />
          </IconButton>
        </header>
        <div class="dialog-body">{props.children}</div>
        {props.footer && <footer class="dialog-foot">{props.footer}</footer>}
      </div>
    </div>
  );
}

/** Placeholder lines while something loads. */
export function Skeleton(props: { lines?: number }): JSX.Element {
  return (
    <div class="skeleton" aria-busy="true" aria-label="Loading">
      {Array.from({ length: props.lines ?? 3 }, (_, i) => (
        <span key={i} class="skeleton-line" />
      ))}
    </div>
  );
}

export function Empty(props: {
  icon: ComponentChildren;
  title: string;
  children?: ComponentChildren;
}): JSX.Element {
  return (
    <div class="empty">
      <div class="empty-icon">{props.icon}</div>
      <p class="empty-title">{props.title}</p>
      {props.children && <div class="empty-text">{props.children}</div>}
    </div>
  );
}

/** ferrobot's own mark: a rounded square with two "eyes" and a spark. */
export function Logo(props: { size?: number }): JSX.Element {
  const s = props.size ?? 28;
  return (
    <svg width={s} height={s} viewBox="0 0 32 32" aria-hidden="true" class="logo">
      <rect x="2" y="5" width="28" height="23" rx="8" class="logo-body" />
      <circle cx="11.5" cy="16.5" r="2.6" class="logo-eye" />
      <circle cx="20.5" cy="16.5" r="2.6" class="logo-eye" />
      <path d="M16 1.5 L17.6 5 L14.4 5 Z" class="logo-spark" />
      <rect x="12" y="22" width="8" height="1.8" rx="0.9" class="logo-eye" />
    </svg>
  );
}
