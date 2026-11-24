import { createContext } from "preact";
import { useContext } from "preact/hooks";

import type { Agent, DaemonClient, Health, Pin, UiEvent } from "./api.ts";
import type { CallState } from "./lib/call.ts";
import type { CallSession } from "./lib/call-session.ts";

export interface AppContextValue {
  client: DaemonClient;
  health: Health | null;
  agents: Map<string, Agent>;
  /** Registers a live-event listener; returns the unsubscribe function. */
  listen: (fn: (e: UiEvent) => void) => () => void;
  toast: (message: string, kind?: "error" | "info") => void;
  /** Opens an agent's chat (e.g. from a mention). */
  openAgent: (id: string) => void;
  /** Opens the read-only thread between two agents. */
  openConversation: (a: string, b: string) => void;
  /** Opens a call room's transcript. */
  openRoom: (id: number) => void;
  /** Opens a file an agent shared (its `file` event id). */
  openFile: (agent: string, file: number) => void;
  /** Pinned replies and files, in the pin list's order. */
  pins: Pin[];
  /** Saves the whole pin list (pin, rename, reorder, unpin). */
  savePins: (pins: Pin[]) => Promise<void>;
  /** Opens an agent's chat scrolled to one of its replies (its event id). */
  jumpTo: (agent: string, event: number) => void;
  /** The operator's voice call; it outlives the chat it started in. */
  call: CallSession;
  /** The call's agents, host and mute state; `null` when there is none. */
  callState: CallState | null;
  /** Runs an action and reports failures as a toast. */
  run: <T>(action: () => Promise<T>) => Promise<T | undefined>;
}

export const AppContext = createContext<AppContextValue | null>(null);

export function useApp(): AppContextValue {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error("AppContext missing");
  return ctx;
}
