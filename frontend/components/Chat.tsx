"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import PlansModal from "./PlansModal";
import { logger } from "../lib/log";

const log = logger("chat");
import { ApiError, api, browserTimezone, setTokenProvider, type ChatMsg, type Quota, type Reminder, type TokenProvider, type Vital } from "../lib/api";
import { T } from "../lib/i18n";
import { LiveSession, tidyCaption } from "../lib/live";
import { useLang } from "../lib/lang";


export default function Chat({ getToken, userMenu, onDeleted }: { getToken: TokenProvider; userMenu: React.ReactNode; onDeleted: () => void }) {
  // Idempotent module-level assignment; done during render so the first API calls (child effects) already have it.

  setTokenProvider(getToken);
  const { lang, setLang } = useLang();
  const [msgs, setMsgs] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState("");
  const [status, setStatus] = useState<"idle" | "thinking">("idle");
  const [error, setError] = useState("");
  const [outOfTokens, setOutOfTokens] = useState(false);
  const [voiceOut, setVoiceOut] = useState(false);
  const [voice, setVoice] = useState<"off" | "connecting" | "listening" | "speaking">("off");
  const [caption, setCaption] = useState({ user: "", assistant: "" });
  const [reminders, setReminders] = useState<Reminder[]>([]);
  const [vitals, setVitals] = useState<Vital[]>([]);
  const [quota, setQuota] = useState<Quota | null>(null);
  const [showPlans, setShowPlans] = useState(false);
  const [docNote, setDocNote] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [acctNote, setAcctNote] = useState("");
  const t = T[lang];

  const endRef = useRef<HTMLDivElement>(null);
  const liveRef = useRef<LiveSession | null>(null);
  const capRef = useRef({ user: "", assistant: "", emergency: "" });

  useEffect(() => { endRef.current?.scrollIntoView({ behavior: "smooth" }); }, [msgs, status]);

  const refreshQuota = useCallback(() => { api.status().then((q) => { setQuota(q); setOutOfTokens(q.remaining <= 0); }).catch((e) => log.warn("could not load plan/usage", e)); }, []);
  const refreshSide = useCallback(async () => {
    try {
      const [r, v] = await Promise.all([api.reminders(), api.vitals()]);
      setReminders(r.reminders); setVitals(v.vitals);
    } catch { /* best-effort */ }
  }, []);

  useEffect(() => {
    api.history().then((h) => setMsgs(h.messages)).catch(() => { });
    // Loading data on mount: the setState calls happen after the fetches resolve, not synchronously.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    refreshSide(); refreshQuota();
    // The server learns our timezone from the first request. If the user has since moved, ask for a correction
    // (the server allows one change per 30 days and simply refuses otherwise, which is fine).
    api.me().then(async (m) => {
      const tz = browserTimezone();
      if (m.subscription.timezone !== tz) { await api.setTimezone(tz).catch(() => { }); refreshQuota(); }
    }).catch(() => { });
  }, [refreshSide, refreshQuota]);

  const handleError = useCallback((e: unknown) => {
    if (e instanceof ApiError && e.status === 402) { setOutOfTokens(true); setError(T[lang].quotaMsg); }
    else setError(e instanceof Error ? e.message : "Something went wrong");
  }, [lang]);

  const send = useCallback(async (text: string) => {
    const message = text.trim();
    if (!message || status === "thinking") return;
    setError(""); setInput("");
    setMsgs((m) => [...m, { role: "user", content: message }]);
    setStatus("thinking");
    try {
      const res = await api.chat(message, lang);
      setMsgs((m) => [...m, { role: "assistant", content: res.reply, emergency: res.emergency }]);
      if (res.tools_used.some((n) => ["create_reminder", "log_vital"].includes(n))) refreshSide();
      refreshQuota();
    } catch (e) { handleError(e); }
    finally { setStatus("idle"); }
  }, [lang, status, refreshSide, refreshQuota, handleError]);

  /** Speech-to-speech conversation: audio goes straight to the model and its voice comes straight back. */
  const startVoice = useCallback(async () => {
    if (liveRef.current) return;
    setError(""); setVoiceOut(false); setVoice("connecting");
    capRef.current = { user: "", assistant: "", emergency: "" }; setCaption({ user: "", assistant: "" });
    const session = new LiveSession();
    liveRef.current = session;
    const cleanup = () => { if (liveRef.current === session) liveRef.current = null; setVoice("off"); setCaption({ user: "", assistant: "" }); refreshQuota(); refreshSide(); };
    try {
      await session.start(getToken, lang, {
        onState: (st) => setVoice(st),
        onUserText: (x) => { capRef.current.user += x; setCaption({ user: capRef.current.user, assistant: capRef.current.assistant }); },
        onAssistantText: (x) => { capRef.current.assistant += x; setCaption({ user: capRef.current.user, assistant: capRef.current.assistant }); },
        onEmergency: (x) => { capRef.current.emergency = x; },
        onTool: (n) => { if (["create_reminder", "log_vital"].includes(n)) refreshSide(); },
        onTurnComplete: () => {
          const c = capRef.current; capRef.current = { user: "", assistant: "", emergency: "" };
          const add: ChatMsg[] = [];
          if (tidyCaption(c.user)) add.push({ role: "user", content: tidyCaption(c.user) });
          if (c.emergency) add.push({ role: "assistant", content: c.emergency, emergency: true });
          else if (tidyCaption(c.assistant)) add.push({ role: "assistant", content: tidyCaption(c.assistant) });
          if (add.length) setMsgs((m) => [...m, ...add]);
          setCaption({ user: "", assistant: "" }); refreshQuota();
        },
        onError: (code, message) => {
          if (code === "voice_quota_exceeded" || code === "voice_limit") { setVoiceOut(true); setError(T[lang].vOut); }
          else if (code === "idle") setError(T[lang].vIdle);
          else setError(message);
        },
        onClosed: cleanup,
      });
    } catch (e) {
      log.warn("could not start voice", e);
      session.stop();
      setError(e instanceof DOMException && e.name === "NotAllowedError" ? T[lang].micBlocked : e instanceof Error ? e.message : "Could not start voice.");
      cleanup();
    }
  }, [getToken, lang, refreshQuota, refreshSide]);

  const stopVoice = useCallback(() => { liveRef.current?.stop(); }, []);
  useEffect(() => () => { liveRef.current?.stop(); }, []);

  async function onFile(e: React.ChangeEvent<HTMLInputElement>) {
    const f = e.target.files?.[0]; e.target.value = "";
    if (!f) return;
    try {
      const r = await api.uploadDoc(f.name, await f.text());
      setDocNote(t.uploaded(r.chunks)); refreshQuota();
    } catch (err) { setDocNote(err instanceof Error ? err.message : "Upload failed"); }
  }

  async function deleteAccount() {
    try { await api.deleteAccount(); onDeleted(); }
    catch (err) { setAcctNote(err instanceof Error ? err.message : "Failed"); setConfirmDelete(false); }
  }

  const resetTime = quota
    ? new Intl.DateTimeFormat(lang === "hi" ? "hi-IN" : "en-IN", { hour: "numeric", minute: "2-digit", timeZone: quota.timezone }).format(new Date(quota.resets_at * 1000))
    : "";

  const pct = quota && quota.limit > 0 ? Math.min(100, Math.round((quota.used / quota.limit) * 100)) : 0;

  const list = { hidden: {}, show: { transition: { staggerChildren: 0.07 } } };
  const pop = { hidden: { opacity: 0, y: 12, scale: 0.92 }, show: { opacity: 1, y: 0, scale: 1 } };
  const section = "mt-[18px]";

  return (
    <div className="mx-auto grid h-dvh max-w-[1150px] grid-cols-[1fr_330px] gap-4 p-4 max-[860px]:grid-cols-1">
      <motion.main className="glass flex min-h-0 flex-col overflow-hidden rounded-3xl" initial={{ opacity: 0, y: 20 }} animate={{ opacity: 1, y: 0 }} transition={{ type: "spring", stiffness: 120, damping: 20 }}>
        <header className="flex flex-wrap items-center gap-2 border-b border-emerald-900/10 px-4 py-3">
          <motion.span className="grid h-10 w-10 place-items-center rounded-2xl bg-linear-to-br from-brand to-brand-2 text-white shadow-lg shadow-brand/40"
            animate={{ scale: [1, 1.12, 1] }} transition={{ duration: 1.6, repeat: Infinity, ease: "easeInOut" }}>♥</motion.span>
          <h1 className="m-0 flex-1 text-lg font-bold">
            <span className="gradient-text">{t.title}</span>
            <small className="block text-xs font-normal text-muted">{t.sub}</small>
          </h1>

          <button className={`chip ${lang === "en" ? "on" : ""}`} onClick={() => setLang("en")}>EN</button>
          <button className={`chip ${lang === "hi" ? "on" : ""}`} onClick={() => setLang("hi")}>हिं</button>
          <button className="chip" onClick={async () => { await api.clear(); setMsgs([]); }}>{t.clear}</button>
          {userMenu}
        </header>

        <div className="flex flex-1 flex-col gap-2.5 overflow-y-auto p-4">
          {msgs.length === 0 && status === "idle" && voice === "off" && (
            <motion.div className="m-auto max-w-sm text-center text-muted" initial={{ opacity: 0, scale: 0.95 }} animate={{ opacity: 1, scale: 1 }}>
              <motion.div className="mx-auto mb-3 grid h-16 w-16 place-items-center rounded-3xl bg-linear-to-br from-brand to-brand-2 text-3xl text-white shadow-xl shadow-brand/40"
                animate={{ y: [0, -8, 0], rotate: [0, 4, -4, 0] }} transition={{ duration: 4, repeat: Infinity, ease: "easeInOut" }}>♥</motion.div>
              <p>{t.empty}</p>
              <motion.div className="mt-3 flex flex-wrap justify-center gap-2" variants={list} initial="hidden" animate="show">
                {t.suggestions.map((s) => (
                  <motion.button key={s} variants={pop} whileHover={{ scale: 1.06, y: -2 }} whileTap={{ scale: 0.95 }} className="chip" onClick={() => send(s)}>{s}</motion.button>
                ))}
              </motion.div>
            </motion.div>
          )}
          <AnimatePresence initial={false}>
            {msgs.map((m, i) => (
              <motion.div key={i} layout className={`msg ${m.role} ${m.emergency ? "emergency" : ""}`}
                initial={{ opacity: 0, y: 14, scale: 0.94 }} animate={{ opacity: 1, y: 0, scale: 1 }} transition={{ type: "spring", stiffness: 320, damping: 26 }}>
                {m.content}</motion.div>
            ))}
            {caption.user && <div key="cap-u" className="msg user opacity-70">{tidyCaption(caption.user)}</div>}
            {caption.assistant && <div key="cap-a" className="msg assistant opacity-70">{tidyCaption(caption.assistant)}</div>}
            {status === "thinking" && (
              <motion.div key="status" className="msg assistant" initial={{ opacity: 0, y: 10 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0 }}>
                <span className="dots"><span /><span /><span /></span>{" "}
                <span className="muted">{t.thinking}</span>
              </motion.div>
            )}
          </AnimatePresence>
          <div ref={endRef} />
        </div>

        <AnimatePresence>
          {outOfTokens && (
            <motion.div key="oot" className="err" initial={{ opacity: 0, height: 0 }} animate={{ opacity: 1, height: "auto" }} exit={{ opacity: 0, height: 0 }}>
              {t.quotaMsg} <button className="link" onClick={() => setShowPlans(true)}>{t.upgrade}</button>
            </motion.div>
          )}
          {error && !outOfTokens && (
            <motion.div key="err" className="err" initial={{ opacity: 0, x: -10 }} animate={{ opacity: 1, x: 0 }} exit={{ opacity: 0 }}>
              {error}{voiceOut && <> <button className="link" onClick={() => setShowPlans(true)}>{t.upgrade}</button></>}
            </motion.div>
          )}
        </AnimatePresence>

        <AnimatePresence>
          {voice !== "off" && (
            <motion.div key="voice" className="flex items-center gap-3 border-t border-emerald-900/10 bg-white/60 px-4 py-3" role="status" aria-live="polite"
              initial={{ opacity: 0, height: 0 }} animate={{ opacity: 1, height: "auto" }} exit={{ opacity: 0, height: 0 }}>
              <motion.span className="grid h-10 w-10 place-items-center rounded-full bg-linear-to-br from-brand to-brand-2 text-white shadow-lg shadow-brand/40"
                animate={voice === "speaking" ? { scale: [1, 1.3, 1] } : voice === "listening" ? { scale: [1, 1.1, 1] } : { opacity: [0.5, 1, 0.5] }}
                transition={{ duration: voice === "speaking" ? 0.7 : 1.6, repeat: Infinity, ease: "easeInOut" }}>{voice === "speaking" ? "🔊" : "🎤"}</motion.span>
              <span className="flex-1 text-sm">{voice === "connecting" ? t.vConnecting : voice === "speaking" ? t.vSpeaking : t.vListening}</span>
              <button type="button" className="chip" onClick={stopVoice}>{t.vEnd}</button>
            </motion.div>
          )}
        </AnimatePresence>

        <form className="flex items-end gap-2 border-t border-emerald-900/10 p-3" onSubmit={(e) => { e.preventDefault(); send(input); }}>
          <div className="relative">
            {voice !== "off" && <span className="absolute inset-0 rounded-full bg-danger/40 animate-ring" />}
            <motion.button type="button" whileHover={{ scale: 1.08 }} whileTap={{ scale: 0.9 }}
              className={`relative h-11 w-11 cursor-pointer rounded-full border text-lg shadow-sm ${voice !== "off" ? "rec border-danger bg-danger text-white" : "border-emerald-900/10 bg-white"}`}
              onClick={voice === "off" ? startVoice : stopVoice} aria-label={voice === "off" ? t.talk : t.talkStop} title={voice === "off" ? t.talk : t.talkStop}>{voice === "off" ? "🎤" : "⏹"}</motion.button>
          </div>
          <textarea rows={1} value={input} placeholder={t.placeholder}
            className="max-h-32 flex-1 resize-none rounded-xl border border-emerald-900/10 bg-white/90 px-3 py-2.5 font-[inherit] outline-none transition focus:border-brand focus:ring-4 focus:ring-brand/15"
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(input); } }} />
          <motion.button className="btn" whileTap={{ scale: 0.92 }} disabled={!input.trim() || status === "thinking"}>{t.send}</motion.button>
        </form>
        <div className="px-4 pb-2.5 text-center text-[11px] text-muted">{t.disclaimer}</div>
      </motion.main>

      <motion.aside className="glass side min-h-0 overflow-y-auto rounded-3xl p-3.5 max-[860px]:hidden" initial={{ opacity: 0, x: 30 }} animate={{ opacity: 1, x: 0 }} transition={{ type: "spring", stiffness: 120, damping: 20, delay: 0.1 }}>
        <section className="usage">
          <div className="mb-2 flex items-center justify-between text-sm">
            <span><b>{quota?.plan.name ?? "…"}</b> {t.plan}</span>
            <button className="chip" onClick={() => setShowPlans(true)}>{t.upgrade}</button>
          </div>
          <div className="bar-track">
            <motion.div className={`bar-fill ${pct >= 90 ? "hot" : ""}`} initial={{ width: 0 }} animate={{ width: `${pct}%` }} transition={{ type: "spring", stiffness: 80, damping: 18 }} />
          </div>
          <small className="muted" data-testid="usage">
            {quota ? `${quota.remaining.toLocaleString()} / ${quota.limit.toLocaleString()} ${t.tokensLeft}` : ""}
          </small>
          {quota && <small className="muted mt-1 block" data-testid="voice-left">🎙 {Math.floor(quota.voice_remaining_secs / 60)}:{String(quota.voice_remaining_secs % 60).padStart(2, "0")} / {quota.voice_limit_secs / 60} min {t.voiceLeft}</small>}
          {quota && <small className="muted mt-1 block" data-testid="resets">{t.resetsAt} {resetTime} · {quota.timezone}</small>}
        </section>

        <section className={section}>
          <h2>{t.reminders}</h2>
          {reminders.length === 0 && <p className="muted">{t.noRem}</p>}
          <AnimatePresence initial={false}>
            {reminders.map((r) => (
              <motion.div key={r.id} layout initial={{ opacity: 0, x: 20 }} animate={{ opacity: 1, x: 0 }} exit={{ opacity: 0 }} className={`row ${r.done ? "done" : ""}`}>
                <input type="checkbox" className="accent-brand" checked={r.done} disabled={r.done}
                  onChange={async () => { await api.completeReminder(r.id); refreshSide(); }} />
                <span>{r.title}<small>{r.when_text}</small></span>
              </motion.div>
            ))}
          </AnimatePresence>
        </section>

        <section className={section}>
          <h2>{t.docs}</h2>
          <motion.label className="chip" whileHover={{ scale: 1.05 }} whileTap={{ scale: 0.95 }}>
            {t.upload}
            <input type="file" accept=".txt,.md,text/plain,text/markdown" hidden onChange={onFile} />
          </motion.label>
          {docNote && <p className="muted">{docNote}</p>}
        </section>

        <section className={section}>
          <h2>{t.account}</h2>
          <p className="muted">{t.accountHint}</p>
          {!confirmDelete
            ? <button className="chip" onClick={() => { setConfirmDelete(true); setAcctNote(""); }}>{t.deleteAcc}</button>
            : (
              <motion.div className="mt-2.5 flex flex-col gap-3.5" initial={{ opacity: 0, y: -8 }} animate={{ opacity: 1, y: 0 }}>
                <p className="muted">{t.deleteWarn}</p>
                <div className="flex gap-2">
                  <button className="btn danger" onClick={deleteAccount}>{t.deleteSure}</button>
                  <button className="chip" onClick={() => setConfirmDelete(false)}>{t.cancel}</button>
                </div>
              </motion.div>
            )}
          {acctNote && <p className="err !p-0">{acctNote}</p>}
        </section>
      </motion.aside>

      <AnimatePresence>
        {showPlans && <PlansModal lang={lang} quota={quota} onClose={() => setShowPlans(false)} onChanged={refreshQuota} />}
      </AnimatePresence>
    </div>
  );
}
