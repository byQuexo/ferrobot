import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";

import { KeyRound, Pencil, Plus, Trash2 } from "lucide-preact";

import type { CliSpec, CliView, ClisView, SignInView } from "../api.ts";
import { useApp } from "../context.ts";
import {
  KNOWN_CLIS,
  type KnownCli,
  accountLabel,
  installLabel,
  knownCli,
  splitList,
} from "../lib/clis.ts";
import { PrefRow } from "./AppearanceSettings.tsx";
import { LinkOut } from "./ProvidersSettings.tsx";
import { Button, Dialog, IconButton } from "./ui.tsx";

const STATUS: Record<ClisView["status"], string> = {
  idle: "",
  building: "Building the image with the Debian packages. This can take a few minutes.",
  ready: "Debian packages reach agents when their computer next starts or wakes.",
  failed: "The image build failed.",
};

/** The CLIs in the agents' computers, each with its install, account and agents. */
export function ClisSettings(): JSX.Element | null {
  const { client, run, agents } = useApp();
  const [view, setView] = useState<ClisView | null>(null);
  const [editing, setEditing] = useState<CliView | "new" | null>(null);
  const [signing, setSigning] = useState<string | null>(null);

  useEffect(() => {
    void run(async () => {
      setView(await client.clis());
    });
  }, []);
  const building = view?.status === "building";
  useEffect(() => {
    if (!building) return;
    const timer = setInterval(() => {
      client.clis().then(setView, () => undefined);
    }, 2000);
    return () => {
      clearInterval(timer);
    };
  }, [building]);

  if (!view) return null;
  if (!view.supported) {
    return (
      <section class="settings-section form">
        <p class="muted small">CLIs need the Docker backend.</p>
      </section>
    );
  }
  const names = (ids: string[] | undefined): string =>
    (ids ?? []).map((id) => agents.get(id)?.name ?? "a deleted agent").join(", ");
  const hint = (cli: CliView): string =>
    [
      installLabel(cli),
      cli.install.kind === "debian" ? "" : names(cli.agents) || "no agents yet",
      accountLabel(cli),
      cli.error ?? "",
    ]
      .filter(Boolean)
      .join(" · ");

  return (
    <section class="settings-section form">
      <ul class="pref-list">
        {view.clis.map((cli) => (
          <PrefRow key={cli.id} title={cli.id} hint={hint(cli)}>
            {cli.account?.sign_in && (
              <Button
                small
                onClick={() => {
                  setSigning(cli.id);
                }}
              >
                Sign in
              </Button>
            )}
            <IconButton
              label={`Edit ${cli.id}`}
              onClick={() => {
                setEditing(cli);
              }}
            >
              <Pencil size={16} />
            </IconButton>
            <IconButton
              label={`Remove ${cli.id} and its token`}
              tone="danger"
              onClick={() =>
                void run(async () => {
                  await client.deleteCli(cli.id);
                  setView(await client.clis());
                })
              }
            >
              <Trash2 size={16} />
            </IconButton>
          </PrefRow>
        ))}
      </ul>
      {view.clis.length === 0 && <p class="muted small">No CLIs yet.</p>}
      <div class="row-actions">
        <Button
          onClick={() => {
            setEditing("new");
          }}
        >
          <Plus size={16} /> Add CLI
        </Button>
        <span class={`hint${view.status === "failed" ? " bad" : ""}`} role="status">
          {STATUS[view.status]} {view.error ?? ""}
        </span>
      </div>
      <span class="hint">
        A token never enters an agent&apos;s computer: the CLI gets a stand-in, and the daemon swaps
        in the real token only for its hosts. Tools that sign each request with a key (aws, gcloud)
        cannot work this way; use a connector for those.
      </span>
      {editing && (
        <CliDialog
          cli={editing === "new" ? null : editing}
          taken={view.clis.map((c) => c.id)}
          onClose={() => {
            setEditing(null);
          }}
          onSaved={(saved, signIn) => {
            setView(saved);
            setEditing(null);
            if (signIn) setSigning(signIn);
          }}
        />
      )}
      {signing && (
        <SignInDialog
          id={signing}
          building={building}
          onClose={() => {
            setSigning(null);
            void run(async () => {
              setView(await client.clis());
            });
          }}
        />
      )}
    </section>
  );
}

type AccountChoice = "none" | "key" | "sign-in";

/** Adds a CLI from the known list or by hand, or edits one. */
function CliDialog(props: {
  cli: CliView | null;
  taken: string[];
  onClose: () => void;
  onSaved: (view: ClisView, signIn: string | null) => void;
}): JSX.Element {
  const { client, run, agents } = useApp();
  const c = props.cli;
  const [id, setId] = useState(c?.id ?? "");
  const [kind, setKind] = useState<CliSpec["install"]["kind"]>(c?.install.kind ?? "npm");
  const [source, setSource] = useState(() => {
    const i = c?.install;
    if (!i) return "";
    return i.kind === "download" ? i.url : i.kind === "script" ? i.script : i.package;
  });
  const [sha256, setSha256] = useState(
    c?.install.kind === "download" ? (c.install.sha256 ?? "") : "",
  );
  const [programs, setPrograms] = useState(c?.programs?.join(", ") ?? "");
  const [choice, setChoice] = useState<AccountChoice>(
    !c?.account ? "none" : c.account.sign_in ? "sign-in" : "key",
  );
  const [key, setKey] = useState("");
  const [hosts, setHosts] = useState(c?.account?.hosts.join(", ") ?? "");
  const [env, setEnv] = useState(c?.account?.env.join(", ") ?? "");
  const [confirm, setConfirm] = useState(c?.account?.confirm ?? true);
  const [login, setLogin] = useState(c?.account?.sign_in?.login.join(" ") ?? "");
  const [tokenCmd, setTokenCmd] = useState(c?.account?.sign_in?.token.join(" ") ?? "");
  const [picked, setPicked] = useState<string[]>(c?.agents ?? []);

  const fill = (k: KnownCli): void => {
    const s = k.spec;
    const i = s.install;
    setId(k.id);
    setKind(i.kind);
    setSource(i.kind === "download" ? i.url : i.kind === "script" ? i.script : i.package);
    setSha256(i.kind === "download" ? (i.sha256 ?? "") : "");
    setPrograms(s.programs?.join(", ") ?? "");
    setChoice(!s.account ? "none" : s.account.sign_in ? "sign-in" : "key");
    setHosts(s.account?.hosts.join(", ") ?? "");
    setEnv(s.account?.env.join(", ") ?? "");
    setConfirm(s.account?.confirm ?? true);
    setLogin(s.account?.sign_in?.login.join(" ") ?? "");
    setTokenCmd(s.account?.sign_in?.token.join(" ") ?? "");
  };
  const install = (): CliSpec["install"] => {
    const v = source.trim();
    switch (kind) {
      case "debian":
        return { kind, package: v };
      case "npm":
        return { kind, package: v };
      case "download":
        return { kind, url: v, sha256: sha256.trim() || null };
      case "script":
        return { kind, script: source };
    }
  };
  const what = {
    debian: "the package",
    npm: "the package",
    download: "the link",
    script: "the script",
  };
  const missing = [
    ...(id.trim() ? [] : ["a name"]),
    ...(source.trim() ? [] : [what[kind]]),
    ...(choice !== "none" && !hosts.trim() ? ["API hosts"] : []),
    ...(choice !== "none" && !env.trim() ? ["token variables"] : []),
    ...(choice === "sign-in" && !login.trim() ? ["login command"] : []),
    ...(choice === "sign-in" && !tokenCmd.trim() ? ["token command"] : []),
  ];
  const save = (): void =>
    void run(async () => {
      const listed = splitList(programs);
      const spec: CliSpec = {
        install: install(),
        // A downloaded binary needs a file name; the CLI's name is the usual one.
        programs: listed.length || kind === "debian" ? listed : [id.trim()],
        account:
          choice === "none"
            ? null
            : {
                hosts: splitList(hosts),
                env: splitList(env),
                confirm,
                sign_in:
                  choice === "sign-in"
                    ? { login: login.trim().split(/\s+/), token: tokenCmd.trim().split(/\s+/) }
                    : null,
              },
        agents: picked,
      };
      const name = id.trim();
      let saved = await client.putCli(name, spec);
      if (choice === "key" && key.trim()) saved = await client.setCliToken(name, key.trim());
      props.onSaved(saved, choice === "sign-in" ? name : null);
    });
  const known = KNOWN_CLIS.filter((k) => !props.taken.includes(k.id));
  const keyUrl = knownCli(id.trim())?.keyUrl;

  return (
    <Dialog
      title={c ? `Edit ${c.id}` : "Add a CLI"}
      onClose={props.onClose}
      footer={
        <>
          {missing.length > 0 && <span class="hint grow">Still needed: {missing.join(", ")}</span>}
          <Button variant="ghost" onClick={props.onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={missing.length > 0} onClick={save}>
            {choice === "sign-in" ? "Save and sign in" : "Save"}
          </Button>
        </>
      }
    >
      <div class="form">
        {!c && known.length > 0 && (
          <span class="field">
            Known CLIs
            <div class="chips">
              {known.map((k) => (
                <button
                  key={k.id}
                  type="button"
                  title={k.about}
                  class={`chip${id === k.id ? " on" : ""}`}
                  onClick={() => {
                    fill(k);
                  }}
                >
                  {k.id}
                </button>
              ))}
            </div>
          </span>
        )}
        <label>
          Name
          <input
            value={id}
            disabled={c !== null}
            maxLength={63}
            spellcheck={false}
            placeholder="e.g. example"
            onInput={(e) => {
              setId(e.currentTarget.value);
            }}
          />
        </label>
        <label>
          Install
          <select
            value={kind}
            onChange={(e) => {
              setKind(e.currentTarget.value as CliSpec["install"]["kind"]);
            }}
          >
            <option value="debian">Debian package (every computer, rebuilds the image)</option>
            <option value="npm">npm package (picked agents)</option>
            <option value="download">Download link (picked agents)</option>
            <option value="script">Install script (picked agents)</option>
          </select>
        </label>
        {kind === "script" ? (
          <label>
            Script
            <textarea
              value={source}
              rows={4}
              spellcheck={false}
              placeholder="Puts the CLI in ~/.local/bin; runs as the agent's user"
              onInput={(e) => {
                setSource(e.currentTarget.value);
              }}
            />
          </label>
        ) : (
          <label>
            {kind === "download" ? "Download link" : "Package"}
            <input
              value={source}
              spellcheck={false}
              placeholder={
                kind === "download" ? "https://example.com/example-linux-arm64" : "example-cli"
              }
              onInput={(e) => {
                setSource(e.currentTarget.value);
              }}
            />
          </label>
        )}
        {kind === "download" && (
          <label>
            SHA-256
            <input
              value={sha256}
              spellcheck={false}
              placeholder="Optional; checked before it is installed"
              onInput={(e) => {
                setSha256(e.currentTarget.value);
              }}
            />
          </label>
        )}
        <label>
          Programs
          <input
            value={programs}
            spellcheck={false}
            placeholder="Empty: the CLI's name"
            onInput={(e) => {
              setPrograms(e.currentTarget.value);
            }}
          />
        </label>
        <fieldset class="conn-kind">
          <legend>Account</legend>
          {(
            [
              ["none", "None"],
              ["key", "API key"],
              ["sign-in", "Sign in with the CLI"],
            ] as [AccountChoice, string][]
          ).map(([value, label]) => (
            <label key={value} class="check">
              <input
                type="radio"
                name="cli-account"
                checked={choice === value}
                onChange={() => {
                  setChoice(value);
                }}
              />
              {label}
            </label>
          ))}
        </fieldset>
        {choice === "key" && (
          <label>
            API key
            <input
              type="password"
              value={key}
              autoComplete="off"
              placeholder={c?.has_token ? "Stored; leave empty to keep it" : "Goes to the vault"}
              onInput={(e) => {
                setKey(e.currentTarget.value);
              }}
            />
            {keyUrl && (
              <span class="hint">
                Create one at <LinkOut label="this page" url={keyUrl} />.
              </span>
            )}
          </label>
        )}
        {choice !== "none" && (
          <details open={!knownCli(id.trim())}>
            <summary>Advanced</summary>
            <label>
              API hosts
              <input
                value={hosts}
                spellcheck={false}
                placeholder="e.g. api.example.com"
                onInput={(e) => {
                  setHosts(e.currentTarget.value);
                }}
              />
              <span class="hint">Only requests to these hosts get the real token.</span>
            </label>
            <label>
              Token variables
              <input
                value={env}
                spellcheck={false}
                placeholder="e.g. EXAMPLE_API_KEY"
                onInput={(e) => {
                  setEnv(e.currentTarget.value);
                }}
              />
            </label>
            {choice === "sign-in" && (
              <>
                <label>
                  Login command
                  <input
                    value={login}
                    spellcheck={false}
                    placeholder="e.g. example auth login --web"
                    onInput={(e) => {
                      setLogin(e.currentTarget.value);
                    }}
                  />
                </label>
                <label>
                  Token command
                  <input
                    value={tokenCmd}
                    spellcheck={false}
                    placeholder="e.g. example auth token"
                    onInput={(e) => {
                      setTokenCmd(e.currentTarget.value);
                    }}
                  />
                  <span class="hint">
                    Runs in a throwaway computer after you sign in; only CLIs that show a one-time
                    code work.
                  </span>
                </label>
              </>
            )}
            <label class="check">
              <input
                type="checkbox"
                checked={confirm}
                onChange={(e) => {
                  setConfirm(e.currentTarget.checked);
                }}
              />
              Ask before every command that uses it
            </label>
          </details>
        )}
        <span class="field">
          Agents
          <div class="chips" role="group" aria-label="Agents with this CLI">
            {[...agents.values()].map((a) => {
              const on = picked.includes(a.id);
              return (
                <button
                  key={a.id}
                  type="button"
                  class={`chip${on ? " on" : ""}`}
                  aria-pressed={on}
                  onClick={() => {
                    setPicked(on ? picked.filter((x) => x !== a.id) : [...picked, a.id]);
                  }}
                >
                  {a.name}
                </button>
              );
            })}
          </div>
          <span class="hint">
            {kind === "debian"
              ? "A Debian package is in every computer; the agents picked here get its account."
              : "Only these agents get it, installed in their home folder; nobody restarts."}
          </span>
        </span>
      </div>
    </Dialog>
  );
}

/** A CLI's own sign-in: its one-time code, then the token goes to the vault. */
function SignInDialog(props: { id: string; building: boolean; onClose: () => void }): JSX.Element {
  const { client, run } = useApp();
  const [state, setState] = useState<SignInView>({ state: "idle" });
  const active = state.state === "starting" || state.state === "waiting";
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => {
      client.signIn(props.id).then(setState, () => undefined);
    }, 1000);
    return () => {
      clearInterval(timer);
    };
  }, [active]);
  const start = (): void =>
    void run(async () => {
      setState(await client.startSignIn(props.id));
    });
  const close = (): void => {
    if (active) void client.cancelSignIn(props.id);
    props.onClose();
  };
  return (
    <Dialog
      title={`Sign in to ${props.id}`}
      onClose={close}
      footer={
        <>
          <Button variant="ghost" onClick={close}>
            {state.state === "done" ? "Close" : "Cancel"}
          </Button>
          {(state.state === "idle" || state.state === "failed") && (
            <Button variant="primary" disabled={props.building} onClick={start}>
              <KeyRound size={16} /> {state.state === "failed" ? "Try again" : "Start"}
            </Button>
          )}
        </>
      }
    >
      <div class="form">
        {state.state === "idle" && (
          <p>
            {props.building
              ? "Waiting for the image build to finish first."
              : `Runs ${props.id}'s own sign-in in a throwaway computer. You approve it in your browser; the token goes to the vault and never to an agent.`}
          </p>
        )}
        {state.state === "starting" && <p>Installing {props.id} and starting its sign-in…</p>}
        {state.state === "waiting" && (
          <p>
            {state.url ? (
              <>
                Open <LinkOut label={state.url} url={state.url} />
              </>
            ) : (
              "Open the sign-in page"
            )}{" "}
            and enter <code class="device-code">{state.code ?? "the code it shows"}</code>. This
            window updates once you approve.
          </p>
        )}
        {state.state === "done" && <p>Signed in. The token is in the vault.</p>}
        {state.state === "failed" && <p class="bad">{state.error}</p>}
      </div>
    </Dialog>
  );
}
