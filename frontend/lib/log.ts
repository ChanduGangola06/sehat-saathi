/** Tiny leveled logger. Debug/info only in development; warnings and errors always. Never pass tokens or message text. */
type Level = "debug" | "info" | "warn" | "error";
const order: Record<Level, number> = { debug: 0, info: 1, warn: 2, error: 3 };
const min: Level = process.env.NODE_ENV === "production" ? "warn" : "debug";

function emit(level: Level, scope: string, msg: string, extra?: unknown) {
  if (order[level] < order[min]) return;
  const line = `[${new Date().toISOString().slice(11, 23)}] [${scope}] ${msg}`;
  const fn = level === "debug" ? console.debug : console[level];
  if (extra === undefined) fn(line); else fn(line, extra);
}

export const logger = (scope: string) => ({
  debug: (m: string, x?: unknown) => emit("debug", scope, m, x),
  info: (m: string, x?: unknown) => emit("info", scope, m, x),
  warn: (m: string, x?: unknown) => emit("warn", scope, m, x),
  error: (m: string, x?: unknown) => emit("error", scope, m, x),
});
