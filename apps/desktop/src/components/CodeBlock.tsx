import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Check, Copy, Maximize2 } from "lucide-preact";

import { useApp } from "../context.ts";
import { Dialog } from "./ui.tsx";

const LABELS = { mermaid: "Mermaid", html: "HTML" } as const;
export type PreviewLang = keyof typeof LABELS;

export const isPreviewLang = (lang: string | undefined): lang is PreviewLang =>
  lang === "mermaid" || lang === "html";

/** A ```mermaid or ```html block: a live preview, with its code a click away. */
export function CodeBlock(props: { lang: PreviewLang; code: string }): JSX.Element {
  const { run } = useApp();
  const [code, setCode] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const label = LABELS[props.lang];
  const view = (big: boolean): JSX.Element =>
    code ? (
      <pre class="code-block">
        <code>{props.code}</code>
      </pre>
    ) : props.lang === "mermaid" ? (
      <Mermaid code={props.code} />
    ) : (
      <HtmlPage code={props.code} big={big} />
    );
  const copy = async (): Promise<void> => {
    const ok = await run(async () => {
      await navigator.clipboard.writeText(props.code);
      return true;
    });
    if (!ok) return;
    setCopied(true);
    setTimeout(() => {
      setCopied(false);
    }, 1500);
  };
  return (
    <div class="block">
      <div class="block-head">
        <span class="block-label">{label}</span>
        <span class="spacer" />
        <span class="block-toggle" role="group" aria-label="View">
          {(["Preview", "Code"] as const).map((v) => (
            <button
              key={v}
              type="button"
              aria-pressed={code === (v === "Code")}
              onClick={() => {
                setCode(v === "Code");
              }}
            >
              {v}
            </button>
          ))}
        </span>
        <button
          type="button"
          class="block-btn"
          title="Copy code"
          aria-label="Copy code"
          onClick={() => void copy()}
        >
          {copied ? <Check size={14} /> : <Copy size={14} />}
        </button>
        <button
          type="button"
          class="block-btn"
          title="Expand"
          aria-label="Expand"
          onClick={() => {
            setExpanded(true);
          }}
        >
          <Maximize2 size={14} />
        </button>
      </div>
      <div class="block-body">{view(false)}</div>
      {expanded && (
        <Dialog
          title={label}
          full
          onClose={() => {
            setExpanded(false);
          }}
        >
          <div class="block-expanded">{view(true)}</div>
        </Dialog>
      )}
    </div>
  );
}

type Rendered = { url: string } | { error: string };

/** The block as a page the daemon serves: in a srcdoc frame the app's CSP would block its styles
 * and scripts. */
function HtmlPage(props: { code: string; big: boolean }): JSX.Element {
  const { client } = useApp();
  const [out, setOut] = useState<Rendered | null>(null);
  useEffect(() => {
    let live = true;
    client.preview(props.code).then(
      (r) => {
        if (live) setOut({ url: `${client.base}${r.url}` });
      },
      (err: unknown) => {
        if (live) setOut({ error: err instanceof Error ? err.message : String(err) });
      },
    );
    return () => {
      live = false;
    };
  }, [props.code]);
  if (!out) return <div class="mermaid-loading" aria-busy="true" />;
  if ("error" in out)
    return (
      <>
        <pre class="code-block">
          <code>{props.code}</code>
        </pre>
        <p class="block-error">Could not show the page: {out.error}</p>
      </>
    );
  return (
    <iframe
      class={props.big ? "block-frame big" : "block-frame"}
      title="HTML preview"
      sandbox="allow-scripts"
      src={out.url}
    />
  );
}

/** The diagram as an image: under the app's CSP an inline SVG would lose its styles, and an
 * image can run nothing. */
function Mermaid(props: { code: string }): JSX.Element {
  const dark = useDarkTheme();
  const [out, setOut] = useState<Rendered | null>(null);
  useEffect(() => {
    let live = true;
    void renderMermaid(props.code, dark).then((r) => {
      if (live) setOut(r);
    });
    return () => {
      live = false;
    };
  }, [props.code, dark]);
  if (!out) return <div class="mermaid-loading" aria-busy="true" />;
  if ("error" in out)
    return (
      <>
        <pre class="code-block">
          <code>{props.code}</code>
        </pre>
        <p class="block-error">Could not draw the diagram: {out.error}</p>
      </>
    );
  return <img class="mermaid-img" alt="Diagram" src={out.url} draggable={false} />;
}

function useDarkTheme(): boolean {
  const isDark = (): boolean => document.documentElement.dataset["theme"] === "dark";
  const [dark, setDark] = useState(isDark);
  useEffect(() => {
    const mo = new MutationObserver(() => {
      setDark(isDark());
    });
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => {
      mo.disconnect();
    };
  }, []);
  return dark;
}

let renders = 0;
let host: HTMLDivElement | null = null;

async function renderMermaid(code: string, dark: boolean): Promise<Rendered> {
  const { default: mermaid } = await import("mermaid");
  // Mermaid measures text in the page and the image draws it, so both use the app's font at 16px.
  const fontFamily = getComputedStyle(document.body).fontFamily;
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: "strict",
    suppressErrorRendering: true,
    htmlLabels: false,
    theme: dark ? "dark" : "default",
    fontFamily,
    themeVariables: { fontFamily, fontSize: "16px" },
  });
  if (!host) {
    host = document.createElement("div");
    host.className = "mermaid-host";
    document.body.append(host);
  }
  const id = `mermaid-${String(++renders)}`;
  try {
    const { svg } = await mermaid.render(id, code, host);
    return { url: svgImage(svg) };
  } catch (err) {
    document.getElementById(`d${id}`)?.remove();
    const message = err instanceof Error ? err.message : String(err);
    return { error: message.split("\n")[0] ?? "unknown error" };
  }
}

/** A data URL for Mermaid's SVG: re-serialized as XML (HTML entities would break an SVG image)
 * and sized from its viewBox, as `width="100%"` gives an image no size. */
function svgImage(markup: string): string {
  const svg = new DOMParser().parseFromString(markup, "text/html").querySelector("svg");
  if (!svg) throw new Error("no diagram");
  const [, , w, h] = (svg.getAttribute("viewBox") ?? "").split(/[\s,]+/);
  if (w && h) {
    svg.setAttribute("width", w);
    svg.setAttribute("height", h);
    svg.removeAttribute("style");
  }
  const xml = new XMLSerializer().serializeToString(svg);
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(xml)}`;
}
