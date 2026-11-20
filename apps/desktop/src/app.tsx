import type { JSX } from "preact";
import { useCallback, useEffect, useMemo, useRef, useState } from "preact/hooks";
import { Bot, KeyRound, Plus, WifiOff } from "lucide-preact";

import { ApiError, DEFAULT_DAEMON, DaemonClient, type Agent, type Health, type UiEvent } from "./api.ts";
import { AgentDialog } from "./components/AgentDialog.tsx";
import { FileViewer } from "./components/FileViewer.tsx";
import { Settings, type SettingsSection } from "./components/Settings.tsx";
import { Chat } from "./components/Chat.tsx";
import { ConversationView, type Pair } from "./components/Conversations.tsx";
import { InfoPanel, type InfoTab } from "./components/InfoPanel.tsx";
import { Sidebar } from "./components/Sidebar.tsx";
import { Button, Empty } from "./components/ui.tsx";
import { AppContext, type AppContextValue } from "./context.ts";
import { applyTheme, followSystem, loadTheme, type ThemePref } from "./lib/theme.ts";

interface Toast {
  id: number;
  message: string;
  kind: "error" | "info";
}

const DAEMON_KEY = "ferrobot.daemon";
const TOKEN_KEY = "ferrobot.token";

declare global {
  interface Window {
    /** Operator token injected by the desktop shell, if it found one. */
    __FERROBOT_TOKEN__?: string;
  }
}

function initialToken(): string {
  return window.__FERROBOT_TOKEN__ ?? localStorage.getItem(TOKEN_KEY) ?? "";
}

export function App(): JSX.Element {
  const [base, setBase] = useState(() => localStorage.getItem(DAEMON_KEY) ?? DEFAULT_DAEMON);
  const [token, setToken] = useState(initialToken);
  const [needsToken, setNeedsToken] = useState(false);
  const client = useMemo(() => new DaemonClient(base, token), [base, token]);
  const [agents, setAgents] = useState<Map<string, Agent>>(new Map());
  const [health, setHealth] = useState<Health | null>(null);
  const [online, setOnline] = useState(false);
  const [selected, setSelected] = useState<string | null>(() => localStorage.getItem("ferrobot.selected"));
  const [pair, setPair] = useState<Pair | null>(null);
  const [dialog, setDialog] = useState<"create" | "edit" | null>(null);
  const [settings, setSettings] = useState<SettingsSection | null>(null);
  const [infoOpen, setInfoOpen] = useState(() => localStorage.getItem("ferrobot.info") !== "closed");
  const [tab, setTab] = useState<InfoTab>("profile");
  const [theme, setTheme] = useState<ThemePref>(loadTheme);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const listeners = useRef(new Set<(e: UiEvent) => void>());
  const themeRef = useRef(theme);

  useEffect(() => {
    themeRef.current = theme;
    applyTheme(theme);
  }, [theme]);
  useEffect(() => followSystem(() => themeRef.current), []);
  useEffect(() => { localStorage.setItem("ferrobot.info", infoOpen ? "open" : "closed"); }, [infoOpen]);
  useEffect(() => { if (selected) localStorage.setItem("ferrobot.selected", selected); }, [selected]);

  const toast = useCallback((message: string, kind: "error" | "info" = "error") => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t.slice(-2), { id, message, kind }]);
    setTimeout(() => { setToasts((t) => t.filter((x) => x.id !== id)); }, 5000);
  }, []);

  const run = useCallback(
    async <T,>(action: () => Promise<T>): Promise<T | undefined> => {
      try {
        return await action();
      } catch (err) {
        toast(err instanceof Error ? err.message : String(err));
        return undefined;
      }
    },
    [toast],
  );

  const refresh = useCallback(async () => {
    try {
      const [h, list] = await Promise.all([client.health(), client.listAgents()]);
      setHealth(h);
      setAgents(new Map(list.map((a) => [a.id, a])));
      setNeedsToken(false);
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) setNeedsToken(true);
      throw err;
    }
  }, [client]);

  // Connect, load, and keep the agent list live.
  useEffect(() => {
    void refresh().catch(() => { setOnline(false); });
    return client.subscribe({
      onStatus: (up) => {
        setOnline(up);
        if (up) void refresh().catch(() => undefined);
      },
      onEvent: (e) => {
        if (e.type === "agent") setAgents((m) => new Map(m).set(e.agent.id, e.agent));
        if (e.type === "agent_deleted") {
          setAgents((m) => {
            const next = new Map(m);
            next.delete(e.agent_id);
            return next;
          });
        }
        for (const fn of listeners.current) fn(e);
      },
    });
  }, [client]);

  // Fall back to the first agent when the selection is gone.
  useEffect(() => {
    if (selected && agents.has(selected)) return;
    const first = [...agents.values()].sort(
      (a, b) => Number(Boolean(b.pinned)) - Number(Boolean(a.pinned)) || a.name.localeCompare(b.name),
    )[0];
    if (first) setSelected(first.id);
    else if (agents.size === 0) setSelected(null);
  }, [agents, selected]);

  const listen = useCallback((fn: (e: UiEvent) => void) => {
    listeners.current.add(fn);
    return () => { listeners.current.delete(fn); };
  }, []);

  const openAgent = useCallback((id: string) => {
    setPair(null);
    setSettings(null);
    setSelected(id);
  }, []);
  const openConversation = useCallback((a: string, b: string) => {
    setSettings(null);
    setPair(a < b ? [a, b] : [b, a]);
  }, []);
  const [file, setFile] = useState<{ agent: string; file: number } | null>(null);
  const openFile = useCallback((agent: string, id: number) => { setFile({ agent, file: id }); }, []);
  const ctx: AppContextValue = { client, health, agents, listen, toast, run, openAgent, openConversation, openFile };
  const agent = selected ? agents.get(selected) : undefined;

  return (
    <AppContext.Provider value={ctx}>
      <div class={`shell${agent && infoOpen && !pair && !settings ? " with-info" : ""}`}>
        <Sidebar
          selected={selected}
          onSelect={openAgent}
          pair={pair}
          onOpenPair={setPair}
          onCreate={() => { setDialog("create"); }}
          onSettings={() => { setSettings(settings ? null : "providers"); }}
          settingsOpen={settings !== null}
          online={online}
          onDeleted={(id) => { if (id === selected) setSelected(null); }}
        />
        {settings ? (
          <Settings section={settings} onSection={setSettings} onClose={() => { setSettings(null); }} theme={theme} onTheme={setTheme} />
        ) : pair ? (
          <ConversationView key={pair.join()} pair={pair} />
        ) : agent ? (
          <Chat
            key={agent.id}
            agent={agent}
            infoOpen={infoOpen}
            onToggleInfo={() => { setInfoOpen(!infoOpen); }}
            onEdit={() => { setDialog("edit"); }}
          />
        ) : (
          <main class="chat chat-none">
            {needsToken ? (
              <TokenForm
                onSave={(t) => {
                  localStorage.setItem(TOKEN_KEY, t);
                  setToken(t);
                }}
              />
            ) : online || agents.size > 0 ? (
              <Empty icon={<Bot size={28} />} title="Create your first agent">
                <p>Each agent gets its own computer, memory and mailbox. Agents can message each other.</p>
                <Button variant="primary" onClick={() => { setDialog("create"); }}>
                  <Plus size={16} /> New agent
                </Button>
              </Empty>
            ) : (
              <Offline base={base} onChange={(b) => { localStorage.setItem(DAEMON_KEY, b); setBase(b); }} />
            )}
          </main>
        )}
        {agent && infoOpen && !pair && !settings && (
          <InfoPanel
            agent={agent}
            tab={tab}
            onTab={setTab}
            onEdit={() => { setDialog("edit"); }}
            onDeleted={() => { setSelected(null); }}
          />
        )}
      </div>
      {file && <FileViewer agent={file.agent} file={file.file} onClose={() => { setFile(null); }} />}
      {(dialog === "create" || dialog === "edit") && (
        <AgentDialog
          agent={dialog === "edit" ? agent : undefined}
          onClose={() => { setDialog(null); }}
          onSaved={(a) => {
            setAgents((m) => new Map(m).set(a.id, a));
            setSelected(a.id);
            setDialog(null);
          }}
        />
      )}
      <div class="toasts" role="status" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} class={`toast toast-${t.kind}`}>{t.message}</div>
        ))}
      </div>
    </AppContext.Provider>
  );
}

function TokenForm(props: { onSave: (token: string) => void }): JSX.Element {
  const [value, setValue] = useState("");
  return (
    <Empty icon={<KeyRound size={28} />} title="The daemon needs its access token">
      <p>
        Paste the token from <code>api-token</code> in the daemon’s data folder (default <code>.ferrobot/</code>).
      </p>
      <form
        class="inline-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (value.trim()) props.onSave(value.trim());
        }}
      >
        <input class="mono" type="password" autoComplete="off" value={value} onInput={(e) => { setValue(e.currentTarget.value); }} />
        <Button type="submit">Connect</Button>
      </form>
    </Empty>
  );
}

function Offline(props: { base: string; onChange: (b: string) => void }): JSX.Element {
  const [value, setValue] = useState(props.base);
  return (
    <Empty icon={<WifiOff size={28} />} title="Can’t reach the ferrobot daemon">
      <p>
        Start it with <code>ferrobotd</code>, or point this app at another address. It retries automatically.
      </p>
      <form
        class="inline-form"
        onSubmit={(e) => {
          e.preventDefault();
          props.onChange(value.trim() || DEFAULT_DAEMON);
        }}
      >
        <input class="mono" value={value} onInput={(e) => { setValue(e.currentTarget.value); }} />
        <Button type="submit">Connect</Button>
      </form>
    </Empty>
  );
}
