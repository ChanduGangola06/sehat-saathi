import { logger } from "./log";

const log = logger("api");
export const API = process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8080";

export type Lang = "en" | "hi";
export type ChatMsg = { role: "user" | "assistant"; content: string; emergency?: boolean };
export type Reminder = { id: number; title: string; when_text: string; done: boolean };
export type Vital = { id: number; kind: string; value: string; unit: string; created_at: string };
export type Plan = { id: string; name: string; price_inr: number; daily_tokens: number; max_documents: number; voice_minutes: number };
/** `resets_at` is unix seconds: the user's next local midnight in `timezone`. */
export type Quota = { plan: Plan; limit: number; used: number; remaining: number; voice_limit_secs: number; voice_used_secs: number; voice_remaining_secs: number; resets_at: number; timezone: string };

export class ApiError extends Error {
  constructor(message: string, public status: number, public code: string) { super(message); }
}

/** Clerk's `getToken`. Set by the signed-in app shell; `skipCache` forces a fresh session token. */
export type TokenProvider = (skipCache?: boolean) => Promise<string | null>;
let tokenProvider: TokenProvider | null = null;
export function setTokenProvider(p: TokenProvider) { tokenProvider = p; }

export const browserTimezone = (): string => {
  try { return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC"; } catch { return "UTC"; }
};

async function parse<T>(res: Response): Promise<T> {
  const body = await res.json().catch(() => ({}));
  if (!res.ok) {
    const b = body as { error?: string; code?: string };
    throw new ApiError(b.error ?? `Request failed (${res.status})`, res.status, b.code ?? "error");
  }
  return body as T;
}

// The session token is short-lived and fetched from Clerk for every request (Clerk caches it briefly), so there is
// nothing to store or refresh here and no cookie is sent to our API.
async function authed(path: string, init: RequestInit = {}, retry = true): Promise<Response> {
  const headers = new Headers(init.headers);
  const token = tokenProvider ? await tokenProvider(!retry) : null;
  if (token) headers.set("Authorization", `Bearer ${token}`);
  headers.set("X-Timezone", browserTimezone());
  const method = init.method ?? "GET";
  const started = performance.now();
  let res: Response;
  try {
    res = await fetch(`${API}${path}`, { ...init, headers });
  } catch (e) {
    log.error(`${method} ${path} network error (is the backend running at ${API}?)`, e);
    throw e;
  }
  const ms = Math.round(performance.now() - started);
  if (res.status === 401 && retry) {
    log.warn(`${method} ${path} -> 401 in ${ms}ms, retrying with a fresh token`);
    return authed(path, init, false); // one retry with a fresh token
  }
  if (res.ok) log.debug(`${method} ${path} -> ${res.status} in ${ms}ms`);
  else log.warn(`${method} ${path} -> ${res.status} in ${ms}ms`);
  return res;
}

const jsonInit = (method: string, body?: unknown): RequestInit => ({
  method,
  headers: { "Content-Type": "application/json" },
  body: body === undefined ? undefined : JSON.stringify(body),
});

export const api = {
  me: async () => parse<{ user: { id: string }; subscription: Quota }>(await authed("/api/me")),
  setTimezone: async (timezone: string) => parse(await authed("/api/timezone", jsonInit("PUT", { timezone }))),
  deleteAccount: async () => { await parse(await authed("/api/account", { method: "DELETE" })); },

  chat: async (message: string, lang: Lang) =>
    parse<{ reply: string; emergency: boolean; tools_used: string[] }>(await authed("/api/chat", jsonInit("POST", { message, lang }))),
  history: async () => parse<{ messages: ChatMsg[] }>(await authed("/api/history")),
  clear: async () => parse(await authed("/api/history", { method: "DELETE" })),

  reminders: async () => parse<{ reminders: Reminder[] }>(await authed("/api/reminders")),
  completeReminder: async (id: number) => parse(await authed(`/api/reminders/${id}/done`, { method: "POST" })),
  vitals: async () => parse<{ vitals: Vital[] }>(await authed("/api/vitals")),
  uploadDoc: async (name: string, text: string) =>
    parse<{ chunks: number }>(await authed("/api/documents", jsonInit("POST", { name, text }))),

  plans: async () => parse<{ plans: Plan[]; dev_mode: boolean }>(await fetch(`${API}/api/billing/plans`)),
  status: async () => parse<Quota>(await authed("/api/billing/status")),
  checkout: async (plan_id: string) =>
    parse<{ order_id: string; amount_inr: number; dev_mode: boolean }>(await authed("/api/billing/checkout", jsonInit("POST", { plan_id }))),
  devActivate: async (order_id: string) => parse(await authed("/api/billing/dev-activate", jsonInit("POST", { order_id }))),
};
