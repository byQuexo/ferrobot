import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { ChevronDown, MessageSquarePlus, Minimize2 } from "lucide-preact";

import type { Agent, AgentContext, Provider } from "../api.ts";
import { useApp } from "../context.ts";
import { contextMeter } from "../lib/agent-meta.ts";
import { modelLabel } from "../lib/models.ts";
import { ModelPicker } from "./ModelPicker.tsx";
import { Button } from "./ui.tsx";

/** The operator's tags on an agent, as small chips. */
export function TagChips(props: { tags?: readonly string[] }): JSX.Element | null {
  if (!props.tags?.length) return null;
  return (
    <span class="tag-chips">
      {props.tags.map((t) => (
        <span key={t} class="tag-chip">
          {t}
        </span>
      ))}
    </span>
  );
}

/** A popover's open state; a press outside `box` or Escape closes it. */
function usePopover(): {
  box: { current: HTMLSpanElement | null };
  open: boolean;
  setOpen: (open: boolean) => void;
  onKeyDown: (e: KeyboardEvent) => void;
} {
  const box = useRef<HTMLSpanElement>(null);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (!open) return;
    const outside = (e: PointerEvent): void => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => {
      document.removeEventListener("pointerdown", outside);
    };
  }, [open]);
  const onKeyDown = (e: KeyboardEvent): void => {
    if (e.key === "Escape") setOpen(false);
  };
  return { box, open, setOpen, onKeyDown };
}

/** The agent's model as a chip; it opens a picker that switches the model. */
export function ModelChip(props: { agent: Agent }): JSX.Element {
  const { client, run } = useApp();
  const a = props.agent;
  const pop = usePopover();
  const [providers, setProviders] = useState<Provider[]>([]);
  const [model, setModel] = useState("");
  const [saving, setSaving] = useState(false);
  useEffect(() => {
    client.listProviders().then(setProviders, () => undefined);
  }, [a.provider_id]);
  const provider = providers.find((p) => p.id === a.provider_id);

  const save = async (): Promise<void> => {
    setSaving(true);
    const saved = await run(() => client.updateAgent(a.id, { model: model.trim() || null }));
    setSaving(false);
    if (saved) pop.setOpen(false);
  };

  return (
    <span class="meta-wrap" ref={pop.box}>
      <button
        type="button"
        class="model-chip"
        title="Switch model"
        aria-expanded={pop.open}
        onClick={() => {
          setModel(a.model ?? "");
          pop.setOpen(!pop.open);
        }}
      >
        <span class="ellipsis">{modelLabel(a, provider)}</span>
        <ChevronDown size={14} aria-hidden="true" />
      </button>
      {pop.open && (
        <div class="meta-pop" role="dialog" aria-label="Switch model" onKeyDown={pop.onKeyDown}>
          <label>
            Model
            <ModelPicker
              provider={a.provider_id ?? undefined}
              kind={provider?.kind ?? "anthropic"}
              value={model}
              placeholder={
                provider?.default_model
                  ? `Default (${provider.default_model})`
                  : "The provider’s default"
              }
              onChange={setModel}
            />
          </label>
          <span class="hint">Switching restarts {a.name}’s computer. Its files stay.</span>
          <div class="meta-pop-actions">
            <Button
              small
              variant="ghost"
              onClick={() => {
                pop.setOpen(false);
              }}
            >
              Cancel
            </Button>
            <Button
              small
              variant="primary"
              disabled={saving || model.trim() === (a.model ?? "")}
              onClick={() => void save()}
            >
              {saving ? "Switching…" : "Switch"}
            </Button>
          </div>
        </div>
      )}
    </span>
  );
}

/** A ring showing how full the agent's context is; it opens the numbers and New chat. */
export function ContextMeter(props: { agent: Agent }): JSX.Element {
  const { client, listen, run } = useApp();
  const id = props.agent.id;
  const pop = usePopover();
  const [ctx, setCtx] = useState<AgentContext | null>(null);
  const [compacting, setCompacting] = useState(false);
  useEffect(() => {
    let live = true;
    const load = (): void => {
      client.agentContext(id).then(
        (c) => {
          if (live) setCtx(c);
        },
        () => undefined,
      );
    };
    setCtx(null);
    load();
    const off = listen((e) => {
      if (
        e.type === "event" &&
        e.event.agent_id === id &&
        (e.event.kind === "turn_done" ||
          e.event.kind === "new_session" ||
          e.event.kind === "compacted")
      ) {
        load();
      }
    });
    return () => {
      live = false;
      off();
    };
  }, [id]);
  const meter = contextMeter(ctx);
  const fill = meter.percent ?? 0;

  return (
    <span class="meta-wrap" ref={pop.box}>
      <button
        type="button"
        class={`ctx-ring${fill >= 80 ? " high" : ""}`}
        aria-label={meter.title}
        title={meter.title}
        aria-expanded={pop.open}
        onClick={() => {
          pop.setOpen(!pop.open);
        }}
      >
        <i style={{ "--p": String(fill) }} />
      </button>
      {pop.open && (
        <div class="meta-pop ctx-pop" role="dialog" aria-label="Context" onKeyDown={pop.onKeyDown}>
          <strong>{meter.title}</strong>
          {meter.percent !== null && (
            <span class="ctx-bar">
              <span style={{ width: `${String(meter.percent)}%` }} />
            </span>
          )}
          <span class="muted small">{meter.detail}</span>
          {ctx?.compact && (
            <>
              <Button
                small
                disabled={compacting}
                onClick={() =>
                  void (async () => {
                    setCompacting(true);
                    await run(() => client.compact(id));
                    setCompacting(false);
                  })()
                }
              >
                <Minimize2 size={16} /> {compacting ? "Compacting…" : "Compact now"}
              </Button>
              <span class="hint">
                {props.agent.name} keeps a summary of the conversation instead of all of it.
              </span>
            </>
          )}
          <Button
            small
            onClick={() =>
              void run(async () => {
                await client.newSession(id);
                pop.setOpen(false);
              })
            }
          >
            <MessageSquarePlus size={16} /> New chat
          </Button>
          <span class="hint">
            {props.agent.name} starts its next turn without the earlier messages. This chat stays.
          </span>
        </div>
      )}
    </span>
  );
}
