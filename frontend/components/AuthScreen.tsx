"use client";

import { T } from "@/lib/i18n";
import { useLang } from "@/lib/lang";
import { SignIn, SignUp } from "@clerk/nextjs";
import { motion, AnimatePresence } from "motion/react";
import { useState } from "react";

/**
 * Clerk's prebuilt screens handle everything credential-related: email + password, email verification code,
 * "forgot password", and the second factor (authenticator app / backup code) when the user has turned it on.
 * `routing="hash"` keeps the steps on this one page, so no extra routes are needed.
 */

export default function AuthScreen() {
    const { lang, setLang } = useLang();
    const t = T[lang];
    const [mode, setMode] = useState<"in" | "up">("in");
    const look = {
        variables: { colorPrimary: "#0f8a74", borderRadius: "0.9rem" },
        elements: { rootBox: { width: "100%" }, cardBox: { width: "100%", maxWidth: 400, boxShadow: "0 20px 50px -20px rgba(15,138,116,.35)" } },
    };

    return (
        <div className="grid min-h-dvh place-items-center p-4">
            <motion.div className="flex w-full max-w-[420px] flex-col items-center gap-4"
                initial={{ opacity: 0, y: 24 }} animate={{ opacity: 1, y: 0 }} transition={{ type: "spring", stiffness: 120, damping: 18 }}>
                <div className="flex w-full items-center justify-between gap-2">
                    <div className="flex items-center gap-3">
                        <motion.span className="grid h-11 w-11 place-items-center rounded-2xl bg-linear-to-br from-brand to-brand-2 text-xl text-white shadow-lg shadow-brand/40"
                            animate={{ scale: [1, 1.12, 1] }} transition={{ duration: 1.6, repeat: Infinity, ease: "easeInOut" }}>♥</motion.span>
                        <h1 className="m-0 text-2xl font-bold">
                            <span className="gradient-text">{t.title}</span>
                            <small className="block text-[13px] font-normal text-muted">{t.sub}</small>
                        </h1>
                    </div>

                    <span>
                        <button className={`chip ${lang === "en" ? "on" : ""}`} onClick={() => setLang("en")}>EN</button>
                        <button className={`chip ${lang === "hi" ? "on" : ""}`} onClick={() => setLang("hi")}>हिं</button>
                    </span>
                </div>

                <AnimatePresence mode="wait">
                    <motion.div key={mode} className="w-full" initial={{ opacity: 0, x: mode === "in" ? -24 : 24 }} animate={{ opacity: 1, x: 0 }} exit={{ opacity: 0, x: mode === "in" ? 24 : -24 }} transition={{ duration: 0.22 }}>
                        {mode === "in" ? <SignIn routing="hash" appearance={look} /> : <SignUp routing="hash" appearance={look} />}
                    </motion.div>
                </AnimatePresence>
                <button className="link" onClick={() => setMode(mode === "in" ? "up" : "in")}>
                    {mode === "in" ? t.noAccount : t.haveAccount}
                </button>
                <p className="muted text-center !text-xs">{t.disclaimer}</p>
            </motion.div>
        </div>
    );

}