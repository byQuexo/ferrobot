import type { JSX } from "preact";

import type { Agent } from "../api.ts";
import { useApp } from "../context.ts";

/** Where to fix an agent's model credential or tools problem, and a retry for fixes made elsewhere. */
export function CredentialFix(props: {
  agent: Agent;
  onEdit: () => void;
  onVault: () => void;
}): JSX.Element {
  const { client, run } = useApp();
  const pickModel = props.agent.provider_id == null || !props.agent.credential_error;
  return (
    <span class="credential-fix">
      <button type="button" class="text-link" onClick={pickModel ? props.onEdit : props.onVault}>
        {pickModel ? "Choose a model" : "Open Vault"}
      </button>
      <button
        type="button"
        class="text-link"
        onClick={() => void run(() => client.retry(props.agent.id))}
      >
        Retry
      </button>
    </span>
  );
}
