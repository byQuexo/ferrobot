import { createContext } from "preact";
import { useContext } from "preact/hooks";

import type { Agent, DaemonClient, Health, UiEvent } from "./api.ts";

export interface AppContextValue {
  client: DaemonClient;
  health: Health | null;
  agents: Map<string, Agent>;
  /** Registers a live-event listener; returns the unsubscribe function. */
  listen: (fn: (e: UiEvent) => void) => () => void;
  toast: (message: string, kind?: "error" | "info") => void;
  /** Runs an action and reports failures as a toast. */
  run: <T>(action: () => Promise<T>) => Promise<T | undefined>;
}

export const AppContext = createContext<AppContextValue | null>(null);

export function useApp(): AppContextValue {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error("AppContext missing");
  return ctx;
}
