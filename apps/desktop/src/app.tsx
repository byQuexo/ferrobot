import type { JSX } from "preact";
import { useCallback, useEffect, useMemo, useRef, useState } from "preact/hooks";
import { loadOrder, orderAgents, pickSelection } from "./lib/order.ts";
import { Bot, KeyRound, Plus, WifiOff } from "lucide-preact";

import {
  ApiError,
  DEFAULT_DAEMON,
  DaemonClient,
  type Agent,
  type Dashboard,
  type Health,
  type Pin,
  type Provider,
  type UiEvent,
} from "./api.ts";
import { AgentDialog } from "./components/AgentDialog.tsx";
import { CallBar } from "./components/CallBar.tsx";
import { DashboardView } from "./components/Dashboards.tsx";
import { FileViewer } from "./components/FileViewer.tsx";
import { QuickSwitcher } from "./components/QuickSwitcher.tsx";
import { Settings, ShortcutList, type SettingsSection } from "./components/Settings.tsx";
import { Chat } from "./components/Chat.tsx";
import { ConversationView, type Pair } from "./components/Conversations.tsx";
import { InfoPanel, type InfoTab } from "./components/InfoPanel.tsx";
import { RoomView } from "./components/Rooms.tsx";
import { Sidebar } from "./components/Sidebar.tsx";
import { Usage } from "./components/Usage.tsx";
import { Welcome } from "./components/Welcome.tsx";
import { Button, Dialog, Empty } from "./components/ui.tsx";
import { AppContext, type AppContextValue } from "./context.ts";
import type { CallState } from "./lib/call.ts";
import { CallSession } from "./lib/call-session.ts";
import {
  applyAppearance,
  followSystem,
  loadAppearance,
  saveAppearance,
  type Appearance,
} from "./lib/appearance.ts";
import { useShortcuts } from "./lib/shortcuts.ts";
import { welcomeStep } from "./lib/welcome.ts";

interface Toast {
  id: number;
  message: string;
  kind: "error" | "info";
}

const DAEMON_KEY = "ferrobot.daemon";
const TOKEN_KEY = "ferrobot.token";
const WELCOME_KEY = "ferrobot.welcome";
const SIDEBAR_KEY = "ferrobot.sidebar";

declare global {
  interface Window {
    /** Operator token injected by the desktop shell, if it found one. */
    __FERROBOT_TOKEN__?: string;
    /** Set by the desktop shell: whether the app updater is set up and it started the daemon. */
    __FERROBOT_SHELL__?: { updater: boolean; ownDaemon: boolean };
  }
}

function initialRail(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_KEY) === "rail";
  } catch {
    return false;
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
  const [loaded, setLoaded] = useState(false);
  const [health, setHealth] = useState<Health | null>(null);
  const [providers, setProviders] = useState<Provider[] | null>(null);
  const [skipped, setSkipped] = useState(() => localStorage.getItem(WELCOME_KEY) === "skipped");
  const [online, setOnline] = useState(false);
  const [selected, setSelected] = useState<string | null>(() =>
    localStorage.getItem("ferrobot.selected"),
  );
  const [pair, setPair] = useState<Pair | null>(null);
  const [room, setRoom] = useState<number | null>(null);
  const [dialog, setDialog] = useState<"create" | "edit" | null>(null);
  const [settings, setSettings] = useState<SettingsSection | null>(null);
  const [guide, setGuide] = useState<string | null>(null);
  const [usage, setUsage] = useState(false);
  const [infoOpen, setInfoOpen] = useState(
    () => localStorage.getItem("ferrobot.info") !== "closed",
  );
  const [rail, setRail] = useState(initialRail);
  const [tab, setTab] = useState<InfoTab>("profile");
  const [look, setLook] = useState<Appearance>(loadAppearance);
  const [switcher, setSwitcher] = useState(false);
  const [shortcuts, setShortcuts] = useState(false);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [pins, setPins] = useState<Pin[]>([]);
  const [formerNames, setFormerNames] = useState<Map<string, string>>(new Map());
  const [dashboards, setDashboards] = useState<Dashboard[]>([]);
  const [board, setBoard] = useState<number | null>(null);
  const [focus, setFocus] = useState<{ agent: string; event: number } | null>(null);
  const listeners = useRef(new Set<(e: UiEvent) => void>());
  const lookRef = useRef(look);

  useEffect(() => {
    lookRef.current = look;
    applyAppearance(look);
    saveAppearance(look);
  }, [look]);
  useEffect(() => followSystem(() => lookRef.current), []);
  useEffect(() => {
    localStorage.setItem("ferrobot.info", infoOpen ? "open" : "closed");
  }, [infoOpen]);
  useEffect(() => {
    try {
      localStorage.setItem(SIDEBAR_KEY, rail ? "rail" : "full");
    } catch {
      // Storage can be unavailable; the sidebar then opens expanded next time.
    }
  }, [rail]);
  useEffect(() => {
    if (selected) localStorage.setItem("ferrobot.selected", selected);
  }, [selected]);

  const toast = useCallback((message: string, kind: "error" | "info" = "error") => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t.slice(-2), { id, message, kind }]);
    setTimeout(() => {
      setToasts((t) => t.filter((x) => x.id !== id));
    }, 5000);
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
      const [h, list, ps] = await Promise.all([
        client.health(),
        client.listAgents(),
        client.listProviders(),
      ]);
      setHealth(h);
      setAgents(new Map(list.map((a) => [a.id, a])));
      setLoaded(true);
      setProviders(ps);
      setNeedsToken(false);
      client.pins().then(setPins, () => undefined);
      client.dashboards().then(setDashboards, () => undefined);
      client.formerAgents().then(
        (list) => {
          setFormerNames(new Map(list.map((f) => [f.id, f.name])));
        },
        () => undefined,
      );
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) setNeedsToken(true);
      throw err;
    }
  }, [client]);

  useEffect(() => {
    void refresh().catch(() => {
      setOnline(false);
    });
    return client.subscribe({
      onStatus: (up) => {
        setOnline(up);
        if (up) void refresh().catch(() => undefined);
      },
      onEvent: (e) => {
        if (e.type === "agent") setAgents((m) => new Map(m).set(e.agent.id, e.agent));
        // A new share may be a dashboard's newest version.
        if (e.type === "event" && e.event.kind === "file")
          client.dashboards().then(setDashboards, () => undefined);
        if (e.type === "agent_deleted") {
          setAgents((m) => {
            const gone = m.get(e.agent_id);
            if (gone) setFormerNames((f) => new Map(f).set(gone.id, gone.name));
            const next = new Map(m);
            next.delete(e.agent_id);
            return next;
          });
        }
        for (const fn of listeners.current) fn(e);
      },
    });
  }, [client]);

  useEffect(() => {
    const next = pickSelection(agents, selected, loaded);
    if (next !== undefined) setSelected(next);
  }, [agents, selected, loaded]);

  const listen = useCallback((fn: (e: UiEvent) => void) => {
    listeners.current.add(fn);
    return () => {
      listeners.current.delete(fn);
    };
  }, []);

  const names = useRef(agents);
  names.current = agents;
  const call = useMemo(
    () => new CallSession(client, toast, (id) => names.current.get(id)?.name),
    [client],
  );
  const [callState, setCallState] = useState<CallState | null>(null);
  useEffect(() => {
    const off = [
      call.subscribe(() => {
        setCallState(call.state);
      }),
      listen((e) => {
        call.handle(e);
      }),
    ];
    return () => {
      for (const fn of off) fn();
      call.hangUp();
    };
  }, [call]);

  const openAgent = useCallback((id: string) => {
    setPair(null);
    setRoom(null);
    setSettings(null);
    setUsage(false);
    setBoard(null);
    setSelected(id);
  }, []);
  const openConversation = useCallback((a: string, b: string) => {
    setSettings(null);
    setUsage(false);
    setRoom(null);
    setBoard(null);
    setPair(a < b ? [a, b] : [b, a]);
  }, []);
  const openRoom = useCallback((id: number) => {
    setSettings(null);
    setUsage(false);
    setPair(null);
    setBoard(null);
    setRoom(id);
  }, []);
  const openDashboard = useCallback((index: number) => {
    setSettings(null);
    setUsage(false);
    setPair(null);
    setRoom(null);
    setBoard(index);
  }, []);
  const closeRoom = useCallback(() => {
    setRoom(null);
  }, []);
  const [file, setFile] = useState<{ agent: string; file: number } | null>(null);
  const openFile = useCallback((agent: string, id: number) => {
    setFile({ agent, file: id });
  }, []);
  const savePins = useCallback(
    async (next: Pin[]) => {
      const saved = await run(() => client.setPins(next));
      if (saved) setPins(saved);
    },
    [client, run],
  );
  const saveDashboards = useCallback(
    async (list: Dashboard[]) => {
      const saved = await run(() => client.setDashboards(list));
      if (!saved) return;
      setDashboards(saved);
      setBoard((b) => (b === null || saved.length === 0 ? null : Math.min(b, saved.length - 1)));
    },
    [client, run],
  );
  const reloadDashboards = useCallback(() => {
    client.dashboards().then(setDashboards, () => undefined);
  }, [client]);
  const jumpTo = useCallback(
    (agent: string, event: number) => {
      openAgent(agent);
      setFocus({ agent, event });
    },
    [openAgent],
  );
  const nameOf = (id: string): string | undefined => {
    const former = formerNames.get(id);
    return agents.get(id)?.name ?? (former === undefined ? undefined : `${former} (deleted)`);
  };
  const ctx: AppContextValue = {
    client,
    health,
    agents,
    formerNames,
    nameOf,
    listen,
    toast,
    run,
    openAgent,
    openConversation,
    openRoom,
    openFile,
    pins,
    savePins,
    jumpTo,
    dashboards,
    saveDashboards,
    openDashboard,
    reloadDashboards,
    call,
    callState,
  };
  const agent = selected ? agents.get(selected) : undefined;
  const openVault = (): void => {
    setUsage(false);
    setSettings("vault");
  };
  const openProviders = (): void => {
    setUsage(false);
    setSettings("providers");
  };
  const agentProvider = providers?.find((p) => p.id === agent?.provider_id);
  const openGuide = (page: string | null): void => {
    setUsage(false);
    setSettings("guides");
    setGuide(page);
  };
  const welcome = welcomeStep({
    agents: agents.size,
    providers,
    daemonKey: health?.anthropic_key ?? false,
    skipped,
  });
  const withInfo =
    agent && infoOpen && !pair && room === null && !settings && !usage && board === null;
  const toggleSettings = (): void => {
    setUsage(false);
    setBoard(null);
    setSettings(settings ? null : "appearance");
  };
  const toggleUsage = (): void => {
    setSettings(null);
    setBoard(null);
    setUsage(!usage);
  };
  useShortcuts((id, n) => {
    switch (id) {
      case "switcher":
        setSwitcher(true);
        break;
      case "new-agent":
        setDialog("create");
        break;
      case "settings":
        toggleSettings();
        break;
      case "usage":
        toggleUsage();
        break;
      case "details":
        setInfoOpen(!infoOpen);
        break;
      case "sidebar":
        setRail(!rail);
        break;
      case "agent": {
        const target = orderAgents([...agents.values()], loadOrder())[(n ?? 1) - 1];
        if (target) openAgent(target.id);
        break;
      }
      case "help":
        setShortcuts(true);
        break;
      case "close":
        if (settings) setSettings(null);
        else if (usage) setUsage(false);
        else if (board !== null) setBoard(null);
        else if (withInfo) setInfoOpen(false);
        break;
    }
  });

  return (
    <AppContext.Provider value={ctx}>
      <div class={`shell${withInfo ? " with-info" : ""}${rail ? " rail" : ""}`}>
        <CallBar />
        <Sidebar
          rail={rail}
          onRail={setRail}
          selected={selected}
          onSelect={openAgent}
          pair={pair}
          onOpenPair={(p) => {
            setUsage(false);
            setRoom(null);
            setPair(p);
          }}
          room={room}
          onOpenRoom={openRoom}
          onCreate={() => {
            setDialog("create");
          }}
          onSettings={toggleSettings}
          onUpdates={() => {
            setUsage(false);
            setSettings("updates");
          }}
          settingsOpen={settings !== null}
          onUsage={toggleUsage}
          usageOpen={usage}
          board={board}
          online={online}
          onDeleted={(id) => {
            if (id === selected) setSelected(null);
          }}
        />
        {settings ? (
          <Settings
            section={settings}
            onSection={setSettings}
            onClose={() => {
              setSettings(null);
            }}
            look={look}
            onLook={setLook}
            guide={guide}
            onGuide={openGuide}
          />
        ) : usage ? (
          <Usage
            onClose={() => {
              setUsage(false);
            }}
          />
        ) : board !== null && dashboards[board] ? (
          <DashboardView
            index={board}
            onIndex={setBoard}
            onClose={() => {
              setBoard(null);
            }}
          />
        ) : room !== null ? (
          <RoomView key={room} id={room} onClose={closeRoom} />
        ) : pair ? (
          <ConversationView key={pair.join()} pair={pair} />
        ) : agent ? (
          <Chat
            key={agent.id}
            agent={agent}
            infoOpen={infoOpen}
            onToggleInfo={() => {
              setInfoOpen(!infoOpen);
            }}
            onEdit={() => {
              setDialog("edit");
            }}
            onVault={openVault}
            onSignIn={
              agentProvider?.kind === "codex" && !agentProvider.has_key ? openProviders : undefined
            }
            onActivities={() => {
              setInfoOpen(true);
              setTab("activities");
            }}
            focus={focus?.agent === agent.id ? focus.event : null}
            onFocused={() => {
              setFocus(null);
            }}
          />
        ) : welcome ? (
          <Welcome
            step={welcome}
            providers={providers ?? []}
            onProviders={setProviders}
            onCreated={(a) => {
              setAgents((m) => new Map(m).set(a.id, a));
              setSelected(a.id);
            }}
            onSkip={() => {
              localStorage.setItem(WELCOME_KEY, "skipped");
              setSkipped(true);
            }}
            onVault={openVault}
            onGuide={openGuide}
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
                <p>
                  Each agent gets its own computer, memory and mailbox. Agents can message each
                  other.
                </p>
                <Button
                  variant="primary"
                  onClick={() => {
                    setDialog("create");
                  }}
                >
                  <Plus size={16} /> New agent
                </Button>
              </Empty>
            ) : (
              <Offline
                base={base}
                onChange={(b) => {
                  localStorage.setItem(DAEMON_KEY, b);
                  setBase(b);
                }}
              />
            )}
          </main>
        )}
        {agent && withInfo && (
          <InfoPanel
            agent={agent}
            tab={tab}
            onTab={setTab}
            onEdit={() => {
              setDialog("edit");
            }}
            onVault={openVault}
            onDeleted={() => {
              setSelected(null);
            }}
          />
        )}
      </div>
      {file && (
        <FileViewer
          agent={file.agent}
          file={file.file}
          onClose={() => {
            setFile(null);
          }}
        />
      )}
      {(dialog === "create" || dialog === "edit") && (
        <AgentDialog
          agent={dialog === "edit" ? agent : undefined}
          onClose={() => {
            setDialog(null);
          }}
          onSaved={(a) => {
            setAgents((m) => new Map(m).set(a.id, a));
            setSelected(a.id);
            setDialog(null);
          }}
        />
      )}
      {switcher && (
        <QuickSwitcher
          onAgent={(id) => {
            setSwitcher(false);
            openAgent(id);
          }}
          onSection={(s) => {
            setSwitcher(false);
            setUsage(false);
            setSettings(s);
          }}
          onClose={() => {
            setSwitcher(false);
          }}
        />
      )}
      {shortcuts && (
        <Dialog
          title="Keyboard shortcuts"
          onClose={() => {
            setShortcuts(false);
          }}
        >
          <ShortcutList />
        </Dialog>
      )}
      <div class="toasts" role="status" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} class={`toast toast-${t.kind}`}>
            {t.message}
          </div>
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
        Paste the token from <code>api-token</code> in the daemon’s data folder (default{" "}
        <code>.ferrobot/</code>).
      </p>
      <form
        class="inline-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (value.trim()) props.onSave(value.trim());
        }}
      >
        <input
          class="mono"
          type="password"
          autoComplete="off"
          value={value}
          onInput={(e) => {
            setValue(e.currentTarget.value);
          }}
        />
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
        Start it with <code>ferrobotd</code>, or point this app at another address. It retries
        automatically.
      </p>
      <form
        class="inline-form"
        onSubmit={(e) => {
          e.preventDefault();
          props.onChange(value.trim() || DEFAULT_DAEMON);
        }}
      >
        <input
          class="mono"
          value={value}
          onInput={(e) => {
            setValue(e.currentTarget.value);
          }}
        />
        <Button type="submit">Connect</Button>
      </form>
    </Empty>
  );
}
