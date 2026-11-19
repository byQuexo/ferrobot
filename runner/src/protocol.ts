// Wire protocol between the runner and ferrobotd.
// Mirror of crates/core/src/protocol.rs; both sides parse protocol/fixtures/.
import { z } from "zod";

export const PROTOCOL_VERSION = 1;

const senderSchema = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("user") }),
  z.object({ kind: z.literal("agent"), id: z.uuid() }),
  z.object({ kind: z.literal("routine"), id: z.number().int() }),
]);
export type Sender = z.infer<typeof senderSchema>;

export const daemonFrameSchema = z.discriminatedUnion("type", [
  z.object({
    type: z.literal("welcome"),
    protocol_version: z.number().int(),
    agent_id: z.uuid(),
    agent_name: z.string().min(1),
    instructions: z.string(),
    resume_session_id: z.string().nullable(),
  }),
  z.object({
    type: z.literal("turn"),
    turn_id: z.string().min(1),
    message_id: z.number().int(),
    sender: senderSchema,
    sender_name: z.string(),
    body: z.string(),
  }),
  z.object({ type: z.literal("interrupt"), turn_id: z.string().min(1) }),
  z.object({ type: z.literal("configure"), instructions: z.string() }),
]);
export type DaemonFrame = z.infer<typeof daemonFrameSchema>;
export type TurnFrame = Extract<DaemonFrame, { type: "turn" }>;
export type WelcomeFrame = Extract<DaemonFrame, { type: "welcome" }>;

export type TurnEvent =
  | { kind: "assistant_text"; text: string }
  /** Streaming fragment; shown live, not persisted. `assistant_text` follows. */
  | { kind: "assistant_delta"; text: string }
  | { kind: "tool_use"; id?: string; name: string; input: unknown }
  | { kind: "tool_result"; id?: string; output: string; is_error: boolean }
  | { kind: "tool_denied"; name: string; reason: string }
  | { kind: "log"; level: "debug" | "info" | "warn" | "error"; message: string };

export type TurnOutcome = "completed" | "interrupted" | "failed";

export type RunnerFrame =
  | {
      type: "hello";
      protocol_version: number;
      runner_version: string;
      driver: string;
      session_id: string | null;
    }
  | { type: "event"; turn_id: string; event: TurnEvent }
  | {
      type: "turn_done";
      turn_id: string;
      outcome: TurnOutcome;
      session_id: string | null;
      result: string | null;
      error: string | null;
    };

/** Parses and validates a daemon frame. Throws on malformed input. */
export function parseDaemonFrame(raw: string): DaemonFrame {
  return daemonFrameSchema.parse(JSON.parse(raw));
}
