import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";

import { Pencil, Plus, Trash2 } from "lucide-preact";

import type { ClisView, LoginSpec, LoginView } from "../api.ts";
import { useApp } from "../context.ts";
import { LOGIN_PRESETS, loginLabel, specOf, splitList } from "../lib/logins.ts";
import { Button, Dialog, IconButton } from "./ui.tsx";

/** Keyless tools that install cleanly on the runner image (Debian bookworm). */
const CATALOG: [string, string][] = [
  ["jq", "JSON"],
  ["sqlite3", "SQLite databases"],
  ["poppler-utils", "PDF text and pages (pdftotext)"],
  ["pandoc", "Document conversion"],
  ["imagemagick", "Images"],
  ["ffmpeg", "Audio and video"],
  ["graphviz", "Diagrams (dot)"],
  ["zip", "Zip archives"],
  ["unzip", "Unpack zip archives"],
  ["tree", "Directory trees"],
  ["python3-pip", "Python with pip"],
  ["python3-venv", "Python virtual environments"],
  ["gh", "GitHub CLI; signs in through a GitHub login below"],
];

/** Debian policy; the daemon checks again. */
const PACKAGE = /^[a-z0-9][a-z0-9+.-]{1,62}$/;

const STATUS: Record<ClisView["status"], string> = {
  idle: "Agents use the plain image.",
  building: "Building the image. This can take a few minutes.",
  ready: "Ready. Agents get these tools when their computer next starts or wakes.",
  failed: "Build failed.",
};

/** One shared image for every agent's computer: the runner image plus Debian packages. */
export function ClisSettings(): JSX.Element | null {
  const { client, run } = useApp();
  const [view, setView] = useState<ClisView | null>(null);
  const [draft, setDraft] = useState<string[]>([]);
  const [other, setOther] = useState("");

  useEffect(() => {
    const load = async (): Promise<void> => {
      const v = await run(() => client.clis());
      if (v) {
        setView(v);
        setDraft(v.packages);
      }
    };
    void load();
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

  const toggle = (pkg: string): void => {
    setDraft(draft.includes(pkg) ? draft.filter((p) => p !== pkg) : [...draft, pkg].sort());
  };
  const addOther = (): void => {
    const pkg = other.trim();
    if (!PACKAGE.test(pkg)) return;
    if (!draft.includes(pkg)) toggle(pkg);
    setOther("");
  };
  const apply = async (): Promise<void> => {
    const saved = await run(() => client.updateClis(draft));
    if (saved) setView(saved);
  };
  const extras = draft.filter((p) => !CATALOG.some(([pkg]) => pkg === p));
  const unchanged = draft.join(" ") === view.packages.join(" ");

  return (
    <section class="settings-section form">
      <span class="field">
        Tools
        <div class="chips" role="group" aria-label="CLIs">
          {[...CATALOG, ...extras.map((p): [string, string] => [p, "Remove"])].map(
            ([pkg, about]) => (
              <button
                key={pkg}
                type="button"
                class={`chip${draft.includes(pkg) ? " on" : ""}`}
                aria-pressed={draft.includes(pkg)}
                title={about}
                onClick={() => {
                  toggle(pkg);
                }}
              >
                {pkg}
              </button>
            ),
          )}
        </div>
        <span class="hint">
          CLIs that read a token from a variable sign in through a login below (GitHub is ready to
          use). Tools that sign each request with a key (aws, gcloud) cannot: keys never enter a
          sandbox. Use a connector for those.
        </span>
      </span>
      <span class="field">
        Other Debian package
        <span class="choice-custom">
          <input
            value={other}
            maxLength={63}
            spellcheck={false}
            autoComplete="off"
            placeholder="e.g. htop"
            onInput={(e) => {
              setOther(e.currentTarget.value);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") addOther();
            }}
          />
          <Button small onClick={addOther} disabled={!PACKAGE.test(other.trim())}>
            Add
          </Button>
        </span>
      </span>
      <div class="row-actions">
        <Button
          variant="primary"
          disabled={building || (unchanged && view.status !== "failed")}
          onClick={() => void apply()}
        >
          Build image
        </Button>
        <span class={`hint${view.status === "failed" ? " bad" : ""}`} role="status">
          {STATUS[view.status]} {view.error ?? ""}
        </span>
      </div>
      <Logins />
    </section>
  );
}

/** Logins for CLIs: agents get a stand-in and the daemon swaps in the token from Vault. */
function Logins(): JSX.Element | null {
  const { client, run, agents } = useApp();
  const [logins, setLogins] = useState<LoginView[] | null>(null);
  const [editing, setEditing] = useState<LoginView | "new" | null>(null);
  useEffect(() => {
    void run(async () => {
      setLogins(await client.logins());
    });
  }, []);
  if (!logins) return null;
  const list = [...agents.values()];
  const save = async (id: string, spec: LoginSpec): Promise<void> => {
    const saved = await client.putLogin(id, spec);
    setLogins((all) => [...(all ?? []).filter((x) => x.id !== id), saved]);
  };
  return (
    <>
      <h3 class="pref-head">Logins</h3>
      <span class="hint">
        For CLIs that read a token from a variable, like gh. The agents you pick get a stand-in; the
        daemon swaps in the real token from Vault → CLI logins only for the login&apos;s hosts, so
        it never enters their computers. Changing a login restarts its agents&apos; computers; their
        files stay.
      </span>
      {logins.map((l) => {
        const picked = l.agents ?? [];
        return (
          <div key={l.id} class="field login">
            <div class="login-head">
              <strong>{loginLabel(l.id)}</strong>
              <span class="muted small mono ellipsis grow">{l.hosts.join(", ")}</span>
              <IconButton
                label={`Edit the ${loginLabel(l.id)} login`}
                onClick={() => {
                  setEditing(l);
                }}
              >
                <Pencil size={16} />
              </IconButton>
              <IconButton
                label={`Remove the ${loginLabel(l.id)} login and its token`}
                tone="danger"
                onClick={() =>
                  void run(async () => {
                    await client.deleteLogin(l.id);
                    setLogins((all) => all?.filter((x) => x.id !== l.id) ?? null);
                  })
                }
              >
                <Trash2 size={16} />
              </IconButton>
            </div>
            <div
              class="chips"
              role="group"
              aria-label={`Agents with the ${loginLabel(l.id)} login`}
            >
              {list.map((a) => {
                const on = picked.includes(a.id);
                return (
                  <button
                    key={a.id}
                    type="button"
                    class={`chip${on ? " on" : ""}`}
                    aria-pressed={on}
                    onClick={() =>
                      void run(() =>
                        save(l.id, {
                          ...specOf(l),
                          agents: on ? picked.filter((x) => x !== a.id) : [...picked, a.id],
                        }),
                      )
                    }
                  >
                    {a.name}
                  </button>
                );
              })}
            </div>
            <span class="hint">
              {l.has_token
                ? "Token stored in Vault."
                : "No token yet: add one in Vault → CLI logins."}
              {l.confirm ? " Asks before every command that uses it, unless Auto mode is on." : ""}
            </span>
          </div>
        );
      })}
      <div class="row-actions">
        <Button
          small
          onClick={() => {
            setEditing("new");
          }}
        >
          <Plus size={16} /> Add login
        </Button>
      </div>
      {editing && (
        <LoginDialog
          login={editing === "new" ? null : editing}
          taken={logins.map((l) => l.id)}
          onClose={() => {
            setEditing(null);
          }}
          onSave={async (id, spec) => {
            await save(id, spec);
            setEditing(null);
          }}
        />
      )}
    </>
  );
}

/** Adds a login from a preset or by hand, or edits one. */
function LoginDialog(props: {
  login: LoginView | null;
  taken: string[];
  onClose: () => void;
  onSave: (id: string, spec: LoginSpec) => Promise<void>;
}): JSX.Element {
  const { run } = useApp();
  const l = props.login;
  const [id, setId] = useState(l?.id ?? "");
  const [hosts, setHosts] = useState(l?.hosts.join(", ") ?? "");
  const [env, setEnv] = useState(l?.env.join(", ") ?? "");
  const [programs, setPrograms] = useState(l?.programs?.join(", ") ?? "");
  const [confirm, setConfirm] = useState(l?.confirm ?? true);
  const usePreset = (preset: (typeof LOGIN_PRESETS)[number]): void => {
    setId(preset.id);
    setHosts(preset.spec.hosts.join(", "));
    setEnv(preset.spec.env.join(", "));
    setPrograms(preset.spec.programs?.join(", ") ?? "");
    setConfirm(preset.spec.confirm ?? false);
  };
  const presets = LOGIN_PRESETS.filter((p) => !props.taken.includes(p.id));
  return (
    <Dialog
      title={l ? `Edit the ${loginLabel(l.id)} login` : "Add a login"}
      onClose={props.onClose}
      footer={
        <>
          <Button variant="ghost" onClick={props.onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            disabled={!id.trim() || !hosts.trim() || !env.trim()}
            onClick={() =>
              void run(() =>
                props.onSave(id.trim(), {
                  hosts: splitList(hosts),
                  env: splitList(env),
                  programs: splitList(programs),
                  confirm,
                  agents: l?.agents ?? [],
                }),
              )
            }
          >
            Save
          </Button>
        </>
      }
    >
      <div class="form">
        {!l && presets.length > 0 && (
          <span class="field">
            Start from
            <div class="chips">
              {presets.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  class={`chip${id === p.id ? " on" : ""}`}
                  onClick={() => {
                    usePreset(p);
                  }}
                >
                  {p.label}
                </button>
              ))}
            </div>
          </span>
        )}
        <label>
          Name
          <input
            value={id}
            disabled={l !== null}
            maxLength={32}
            spellcheck={false}
            placeholder="e.g. example"
            onInput={(e) => {
              setId(e.currentTarget.value);
            }}
          />
          <span class="hint">Lowercase letters, digits and dashes.</span>
        </label>
        <label>
          API hosts
          <input
            value={hosts}
            spellcheck={false}
            placeholder="api.example.com"
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
            placeholder="EXAMPLE_API_KEY"
            onInput={(e) => {
              setEnv(e.currentTarget.value);
            }}
          />
          <span class="hint">Where the CLI looks for its token; agents get a stand-in there.</span>
        </label>
        <label>
          Programs
          <input
            value={programs}
            spellcheck={false}
            placeholder="example"
            onInput={(e) => {
              setPrograms(e.currentTarget.value);
            }}
          />
        </label>
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
        <span class="hint">
          A card for each command that runs one of the programs or names a variable or host, unless
          Auto mode is on. A script can still reach the login without naming it.
        </span>
      </div>
    </Dialog>
  );
}
