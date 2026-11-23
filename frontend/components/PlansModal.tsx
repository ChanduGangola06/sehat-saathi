"use client";

import { motion } from "motion/react";
import { useEffect, useState } from "react";
import { api, type Plan, type Quota, type Lang } from "../lib/api";
import { T } from "../lib/i18n";
import { logger } from "../lib/log";

const log = logger("plans");

export default function PlansModal({ lang, quota, onClose, onChanged }: { lang: Lang; quota: Quota | null; onClose: () => void; onChanged: () => void }) {
  const t = T[lang];
  const [plans, setPlans] = useState<Plan[]>([]);
  const [dev, setDev] = useState(false);
  const [busy, setBusy] = useState("");
  const [note, setNote] = useState("");
  const [error, setError] = useState("");

  useEffect(() => { api.plans().then((r) => { setPlans(r.plans); setDev(r.dev_mode); }).catch((e) => { log.error("could not load plans", e); setError("Could not load plans"); }); }, []);

  async function upgrade(p: Plan) {
    setBusy(p.id); setError(""); setNote("");
    try {
      const order = await api.checkout(p.id);
      if (order.dev_mode) {
        await api.devActivate(order.order_id);
        setNote(t.activated);
        onChanged();
      } else {
        setNote(t.payNotReady); // hand `order.order_id` to your payment gateway here
      }
    } catch (e) { log.error(`upgrade to ${p.id} failed`, e); setError(e instanceof Error ? e.message : "Failed"); }
    finally { setBusy(""); }
  }

  return (
    <motion.div className="fixed inset-0 z-10 grid place-items-center bg-slate-900/40 p-4 backdrop-blur-sm" onClick={onClose} role="dialog" aria-modal="true"
      initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }}>
      <motion.div className="glass flex max-h-[90dvh] w-full max-w-3xl flex-col gap-3 overflow-y-auto rounded-3xl p-5" onClick={(e) => e.stopPropagation()}
        initial={{ opacity: 0, scale: 0.9, y: 30 }} animate={{ opacity: 1, scale: 1, y: 0 }} exit={{ opacity: 0, scale: 0.95, y: 20 }}
        transition={{ type: "spring", stiffness: 260, damping: 24 }}>
        <div className="flex items-center justify-between">
          <h2 className="m-0 text-xl font-bold"><span className="gradient-text">{t.plans}</span></h2>
          <button className="chip" onClick={onClose}>{t.close}</button>
        </div>
        <div className="grid grid-cols-[repeat(auto-fit,minmax(190px,1fr))] gap-3">
          {plans.map((p, i) => {
            const current = quota?.plan.id === p.id;
            return (
              <motion.div key={p.id} className={`plan ${current ? "current" : ""}`}
                initial={{ opacity: 0, y: 24 }} animate={{ opacity: 1, y: 0 }} transition={{ delay: 0.08 * i, type: "spring", stiffness: 200, damping: 20 }}
                whileHover={{ y: -6, boxShadow: "0 18px 36px -14px rgba(15,138,116,.4)" }}>
                <h3 className="m-0 text-lg font-semibold">{p.name}</h3>
                <div className="price">{p.price_inr === 0 ? t.free : `₹${p.price_inr}`}<small>{p.price_inr === 0 ? "" : t.perMonth}</small></div>
                <ul>
                  <li>{p.daily_tokens.toLocaleString()} {t.tokensDay}</li>
                  <li>{p.max_documents} {t.docsMax}</li>
                  <li>{p.voice_minutes} {t.voiceMinDay}</li>
                </ul>
                {current ? <span className="chip on self-start">{t.current}</span>
                  : p.price_inr > 0 && (
                    <motion.button className="btn" disabled={!!busy} onClick={() => upgrade(p)} whileTap={{ scale: 0.95 }}>
                      {busy === p.id ? t.working : dev ? `${t.upgrade} (${t.devPay})` : t.upgrade}
                    </motion.button>
                  )}
              </motion.div>
            );
          })}
        </div>
        {note && <p className="muted">{note}</p>}
        {error && <div className="err">{error}</div>}
      </motion.div>
    </motion.div>
  );
}
