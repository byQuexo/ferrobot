type Level = "debug" | "info" | "warn" | "error";

const order: Record<Level, number> = { debug: 10, info: 20, warn: 30, error: 40 };
const threshold = order[(process.env["FERROBOT_LOG"] as Level | undefined) ?? "info"];

function write(level: Level, msg: string, fields?: Record<string, unknown>): void {
  if (order[level] < threshold) return;
  process.stderr.write(
    `${JSON.stringify({ ts: new Date().toISOString(), level, msg, ...fields })}\n`,
  );
}

export const log = {
  debug: (msg: string, fields?: Record<string, unknown>) => {
    write("debug", msg, fields);
  },
  info: (msg: string, fields?: Record<string, unknown>) => {
    write("info", msg, fields);
  },
  warn: (msg: string, fields?: Record<string, unknown>) => {
    write("warn", msg, fields);
  },
  error: (msg: string, fields?: Record<string, unknown>) => {
    write("error", msg, fields);
  },
};

export function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
