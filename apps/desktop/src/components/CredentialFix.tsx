import type { JSX } from "preact";

import type { Agent } from "../api.ts";
import { useApp } from "../context.ts";

/** Where to fix an agent's model credential or tools problem, and a retry for fixes made elsewhere. */
export function CredentialFix(props: {
  agent: Agent;
  onEdit: () => void;
  onVault: () => void;
  onSignIn?: () => void;
}): JSX.Element {
  const { client, run } = useApp();
  const pickModel = props.agent.provider_id == null || !props.agent.credential_error;
  const [label, fix] = pickModel
    ? ["Choose a model", props.onEdit]
    : props.onSignIn
      ? ["Sign in with ChatGPT", props.onSignIn]
      : ["Open Vault", props.onVault];
  return (
    <span class="credential-fix">
      <button type="button" class="text-link" onClick={fix}>
        {label}
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
