import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";

import type { ModelInfo, ProviderKind } from "../api.ts";
import { useApp } from "../context.ts";
import { modelOptions } from "../lib/models.ts";
import { Choice } from "./ui.tsx";

/** A model field: the provider's models (or the kind's suggestions before it exists) to pick from, or any id typed in. */
export function ModelPicker(props: {
  provider?: string;
  kind: ProviderKind;
  value: string;
  placeholder: string;
  onChange: (v: string) => void;
}): JSX.Element {
  const { client } = useApp();
  const [models, setModels] = useState<ModelInfo[]>([]);
  const source = props.provider ?? props.kind;
  useEffect(() => {
    let live = true;
    setModels([]);
    const list = props.provider ? client.providerModels(props.provider) : client.kindModels(props.kind);
    list.then((l) => { if (live) setModels(l.models); }, () => undefined);
    return () => { live = false; };
  }, [source]);
  return (
    <Choice
      key={source}
      value={props.value}
      options={modelOptions(models, props.value)}
      placeholder={props.placeholder}
      onChange={props.onChange}
    />
  );
}
