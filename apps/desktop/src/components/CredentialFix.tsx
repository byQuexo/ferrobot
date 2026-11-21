import type { JSX } from "preact";

import type { Agent } from "../api.ts";
import { useApp } from "../context.ts";

/** Where to fix an agent's model credential, and a retry for fixes made elsewhere. */
export function CredentialFix(props: {
  agent: Agent;
  onEdit: () => void;
  onVault: () => void;
}): JSX.Element {
  const { client, run } = useApp();
  const daemonDefault = props.agent.provider_id == null;
  return (
    <span class="credential-fix">
      <button
        type="button"
        class="text-link"
        onClick={daemonDefault ? props.onEdit : props.onVault}
      >
        {daemonDefault ? "Choose a model" : "Open Vault"}
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
