import type { ComponentChildren, JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { Check, Sunrise } from "lucide-preact";

import type { Agent, Provider } from "../api.ts";
import { useApp } from "../context.ts";
import { deriveCharacter, type Character } from "../lib/character.ts";
import {
  STARTER_DESCRIPTION,
  STARTER_INSTRUCTIONS,
  providerReady,
  STARTER_NAME,
  type WelcomeStep,
} from "../lib/welcome.ts";
import { Avatar } from "./Avatar.tsx";
import { ModelPicker } from "./ModelPicker.tsx";
import { ChatgptLogin, ProvidersSettings } from "./ProvidersSettings.tsx";
import { Button } from "./ui.tsx";
import { VaultSettings } from "./VaultSettings.tsx";

const DOCKER_URL = "https://www.docker.com/products/docker-desktop/";

function Line(props: {
  look: Character;
  wide?: boolean;
  children: ComponentChildren;
}): JSX.Element {
  return (
    <div class="row row-agent">
      <Avatar
        agent={{ name: "", avatar_url: null, character: props.look }}
        size={28}
        mood="sleeping"
      />
      <div class={`bubble-agent welcome-line${props.wide ? " wide" : ""}`}>{props.children}</div>
    </div>
  );
}

/**
 * First run: a starter agent that does not exist yet walks the operator through Docker and a
 * model, then is created and takes over in its normal chat.
 */
export function Welcome(props: {
  step: WelcomeStep;
  providers: Provider[];
  onProviders: (providers: Provider[]) => void;
  onCreated: (agent: Agent) => void;
  onSkip: () => void;
  onVault: () => void;
  onGuide: (page: string) => void;
}): JSX.Element {
  const { client, health } = useApp();
  const [look] = useState(() => deriveCharacter(crypto.randomUUID()));
  const name = STARTER_NAME;
  const [model, setModel] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ready, setReady] = useState(health?.sandbox_ready ?? true);
  // Wakes by itself only when the model was added here; opened at "meet", it waits for a click.
  const auto = useRef(props.step !== "meet");
  const [signedIn, setSignedIn] = useState<string | null>(null);
  const codex = props.providers.find((p) => p.kind === "codex" && !p.has_key);
  const provider =
    props.providers.find(providerReady) ?? (codex?.id === signedIn ? codex : undefined);
  // Only the welcome knows about a ChatGPT sign-in, so it moves on to waking by itself.
  const step = props.step === "key" && provider ? "meet" : props.step;
  const shownName = "Ferro";

  useEffect(() => {
    if (ready) return;
    const timer = setInterval(() => {
      client.health().then(
        (h) => {
          setReady(h.sandbox_ready);
        },
        () => undefined,
      );
    }, 3000);
    return () => {
      clearInterval(timer);
    };
  }, [ready]);

  const wake = async (): Promise<void> => {
    setBusy(true);
    setError(null);
    let agent: Agent;
    try {
      agent = await client.createAgent({
        name,
        description: STARTER_DESCRIPTION,
        instructions: STARTER_INSTRUCTIONS,
        character: look,
        provider_id: provider?.id ?? null,
        model: model.trim() || null,
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setBusy(false);
      return;
    }
    // The main agent stays first in the list. The daemon opens its first turn with a greeting.
    agent = await client.updateAgent(agent.id, { pinned: true }).catch(() => agent);
    props.onCreated(agent);
  };

  useEffect(() => {
    if (!auto.current || props.step !== "meet" || !ready) return;
    auto.current = false;
    void wake();
  }, [props.step, ready, name]);

  const guide = (page: string, label: string): JSX.Element => (
    <button
      type="button"
      class="text-link"
      onClick={() => {
        props.onGuide(page);
      }}
    >
      {label}
    </button>
  );

  return (
    <section class="chat welcome" aria-label="Welcome">
      <header class="chat-head">
        <Avatar
          agent={{ name: shownName, avatar_url: null, character: look }}
          size={40}
          presence="sleeping"
        />
        <div class="chat-title">
          <h1>{shownName}</h1>
          <span class="chat-sub p-sleeping">
            <span class="dot" />
            Asleep · your main agent
          </span>
        </div>
        <span class="spacer" />
        <Button variant="ghost" small onClick={props.onSkip}>
          Skip, I’ll set up alone
        </Button>
      </header>
      <div class="timeline">
        <div class="chat-empty">
          <Avatar
            agent={{ name: shownName, avatar_url: null, character: look }}
            size={64}
            mood="sleeping"
          />
          <p class="empty-title">Welcome to ferrobot</p>
        </div>
        <Line look={look}>
          <p>
            Hi, I’m {shownName}, your main agent. I help you set up ferrobot, then I take your tasks
            and build a team of agents when a job needs one. I wake up once I have a model to think
            with.
          </p>
          <p>
            In ferrobot every agent gets its own computer, a mailbox and a memory. Agents work on
            their own, talk to each other and ask you before anything big.
          </p>
        </Line>
        <Line look={look}>
          {ready ? (
            <p class="welcome-done">
              <Check size={16} />{" "}
              {health?.backend === "docker"
                ? "Docker is running, so I have a computer to work on."
                : "My computer is ready."}
            </p>
          ) : (
            <>
              <p>
                Docker is not running. Every agent works in its own container, so start Docker
                Desktop and I’ll notice when it’s up.
              </p>
              <p>
                <a href={DOCKER_URL} target="_blank" rel="noreferrer">
                  Download Docker Desktop
                </a>{" "}
                · {guide("Troubleshooting", "Troubleshooting")}
              </p>
            </>
          )}
        </Line>
        {step === "meet" ? (
          <Line look={look}>
            <p class="welcome-done">
              <Check size={16} /> I’ll think with{" "}
              {provider
                ? `${provider.name}${provider.default_model ? ` (${provider.default_model})` : ""}`
                : "the daemon’s Anthropic key"}
              .
            </p>
          </Line>
        ) : (
          <Line look={look} wide>
            <p>
              I need a model to think with. Add a model provider: pick its kind, give it a name and
              a default model. {guide("Providers", "Guide")}
            </p>
            <ProvidersSettings
              onVault={props.onVault}
              onChange={props.onProviders}
              signIn={false}
            />
          </Line>
        )}
        {step === "key" && (
          <Line look={look} wide>
            <p>
              Now its API key. It stays in the keychain of the daemon’s computer; I never see it.
            </p>
            <VaultSettings providersOnly onProviders={props.onProviders} />
          </Line>
        )}
        {step === "key" && codex && (
          <Line look={look} wide>
            <p>
              Or I think through your ChatGPT account instead of a key. That needs Codex installed
              on this computer; then sign in here.
            </p>
            <ChatgptLogin
              provider={codex}
              onStatus={(s) => {
                setSignedIn(s.state === "signed_in" ? codex.id : null);
              }}
            />
          </Line>
        )}
        {step === "meet" && (
          <Line look={look} wide>
            <p>All set. Wake me up and I’ll introduce myself.</p>
            {provider && !provider.default_model && (
              <label class="welcome-model">
                Model
                <ModelPicker
                  provider={provider.id}
                  kind={provider.kind}
                  value={model}
                  placeholder="The provider’s default"
                  onChange={setModel}
                />
              </label>
            )}
            {error && <p class="hint bad">{error}</p>}
            <div class="form-actions">
              <Button variant="primary" disabled={busy || !ready} onClick={() => void wake()}>
                <Sunrise size={16} /> {busy ? "Waking up…" : `Wake ${shownName}`}
              </Button>
            </div>
          </Line>
        )}
      </div>
    </section>
  );
}
