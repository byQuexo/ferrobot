import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { ArrowUp, File, Folder, FolderOpen, RefreshCw, SquareTerminal } from "lucide-preact";

import type { ExecOutput } from "../api.ts";
import { useApp } from "../context.ts";
import {
  CONSOLE_TIMEOUT_S,
  HOME,
  type Listing,
  VIEW_BYTES,
  consoleCommand,
  listCommand,
  parentOf,
  parseConsole,
  parseListing,
  readCommand,
  shortPath,
  timedOut,
  treeRows,
} from "../lib/computer.ts";
import { errorText } from "../lib/format.ts";
import { fileSize } from "./FileViewer.tsx";
import { Dialog, IconButton, Skeleton } from "./ui.tsx";

const MAX_SHOWN = 500;

/** Browses the agent's computer from its home folder as a tree; files open read-only. */
export function Files(props: { agent: string }): JSX.Element {
  const { client, run } = useApp();
  const [root, setRoot] = useState(HOME);
  const [tick, setTick] = useState(0);
  const [listings, setListings] = useState<ReadonlyMap<string, Listing>>(new Map());
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  const [opened, setOpened] = useState<{ path: string; text: string; cut: boolean } | null>(null);

  const load = (dir: string): void => {
    void client
      .exec(props.agent, listCommand(dir))
      .then(
        (out): Listing =>
          out.exit_code === 0
            ? parseListing(out.stdout)
            : { error: out.stderr.trim() || "This folder cannot be read." },
        (err: unknown): Listing => ({ error: errorText(err) }),
      )
      .then((listing) => {
        setListings((m) => new Map(m).set(dir, listing));
      });
  };

  useEffect(() => {
    load(root);
    for (const dir of expanded) load(dir);
  }, [props.agent, root, tick]);

  const toggle = (dir: string): void => {
    const next = new Set(expanded);
    if (!next.delete(dir)) {
      next.add(dir);
      if (!listings.has(dir)) load(dir);
    }
    setExpanded(next);
  };

  const open = (path: string, size: number): void => {
    void run(async () => {
      const out = await client.exec(props.agent, readCommand(path));
      if (out.exit_code !== 0) throw new Error(out.stderr.trim() || "This file cannot be read.");
      const text = out.stdout.includes("\0") ? "Binary file, not shown." : out.stdout;
      setOpened({ path, text, cut: size > VIEW_BYTES });
    });
  };

  return (
    <section class="card">
      <h3>
        <FolderOpen size={16} /> Files
      </h3>
      <div class="files-bar">
        <IconButton
          label="Up one folder"
          disabled={root === "/"}
          onClick={() => {
            setExpanded(new Set());
            setRoot(parentOf(root));
          }}
        >
          <ArrowUp size={16} />
        </IconButton>
        <span class="mono ellipsis grow" title={root}>
          {shortPath(root)}
        </span>
        <IconButton
          label="Refresh"
          onClick={() => {
            setTick((t) => t + 1);
          }}
        >
          <RefreshCw size={16} />
        </IconButton>
      </div>
      {listings.has(root) ? (
        <ul class="list files">
          {treeRows(root, listings, expanded, MAX_SHOWN).map((row) => (
            <li key={row.key} style={{ paddingLeft: `${row.depth * 16}px` }}>
              {"note" in row ? (
                <span class="muted small">{row.note}</span>
              ) : (
                <button
                  type="button"
                  class="file-row"
                  aria-expanded={row.entry.dir ? row.open : undefined}
                  onClick={() => {
                    if (row.entry.dir) toggle(row.path);
                    else open(row.path, row.entry.size);
                  }}
                >
                  {row.open ? (
                    <FolderOpen size={16} />
                  ) : row.entry.dir ? (
                    <Folder size={16} />
                  ) : (
                    <File size={16} />
                  )}
                  <span class="grow ellipsis">{row.entry.name}</span>
                  {!row.entry.dir && <span class="muted small">{fileSize(row.entry.size)}</span>}
                </button>
              )}
            </li>
          ))}
        </ul>
      ) : (
        <Skeleton lines={3} />
      )}
      {opened && (
        <Dialog
          title={shortPath(opened.path)}
          wide
          onClose={() => {
            setOpened(null);
          }}
        >
          <pre class="file-text">{opened.text}</pre>
          {opened.cut && <p class="muted small">Showing the first {fileSize(VIEW_BYTES)}.</p>}
        </Dialog>
      )}
    </section>
  );
}

interface Run {
  id: number;
  cmd: string;
  cwd: string;
  out?: ExecOutput;
  failed?: string;
}

/** One command at a time in the agent's computer, each with a time limit. */
export function Console(props: { agent: string }): JSX.Element {
  const { client } = useApp();
  const [cwd, setCwd] = useState(HOME);
  const [runs, setRuns] = useState<Run[]>([]);
  const [line, setLine] = useState("");
  const output = useRef<HTMLDivElement>(null);
  const busy = runs.some((r) => !r.out && !r.failed);

  useEffect(() => {
    const el = output.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [runs]);

  const finish = (id: number, done: Partial<Run>): void => {
    setRuns((rs) => rs.map((r) => (r.id === id ? { ...r, ...done } : r)));
  };
  const submit = (): void => {
    const cmd = line.trim();
    if (!cmd || busy) return;
    const id = Date.now();
    setLine("");
    setRuns((rs) => [...rs.slice(-49), { id, cmd, cwd }]);
    client.exec(props.agent, consoleCommand(cmd), cwd).then(
      (out) => {
        const { stdout, cwd: after } = parseConsole(out);
        if (after) setCwd(after);
        finish(id, { out: { ...out, stdout } });
      },
      (err: unknown) => {
        finish(id, { failed: errorText(err) });
      },
    );
  };

  return (
    <section class="card">
      <h3>
        <SquareTerminal size={16} /> Console
      </h3>
      <p class="muted small">
        Runs a command in this computer as the agent's user, for up to {CONSOLE_TIMEOUT_S} s.
        Programs that need a full terminal, like vim, do not work here.
      </p>
      {runs.length > 0 && (
        <div class="console-out" ref={output}>
          {runs.map((r) => (
            <div key={r.id} class="console-run">
              <div>
                <span class="muted">{shortPath(r.cwd)} $</span> {r.cmd}
              </div>
              {!r.out && !r.failed && <div class="muted">Running…</div>}
              {r.failed && <div class="console-err">{r.failed}</div>}
              {r.out && (
                <>
                  {r.out.stdout && <pre>{r.out.stdout}</pre>}
                  {r.out.stderr && <pre class="console-err">{r.out.stderr}</pre>}
                  {timedOut(r.out) ? (
                    <div class="console-err">Stopped after {CONSOLE_TIMEOUT_S} s.</div>
                  ) : (
                    r.out.exit_code !== 0 && <div class="muted">Exit code {r.out.exit_code}</div>
                  )}
                  {r.out.truncated && <div class="muted">Output cut at 1 MB.</div>}
                </>
              )}
            </div>
          ))}
        </div>
      )}
      <form
        class="console-in"
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <span class="mono muted">{shortPath(cwd)} $</span>
        <input
          class="mono"
          aria-label="Command"
          placeholder="ls -la"
          value={line}
          spellcheck={false}
          autocomplete="off"
          autocapitalize="off"
          onInput={(e) => {
            setLine(e.currentTarget.value);
          }}
        />
      </form>
    </section>
  );
}
