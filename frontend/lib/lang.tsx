"use client";

import { useRouter } from "next/navigation";
import { createContext, useCallback, useContext, useEffect, useState } from "react";
import type { Lang } from "./api";

const Ctx = createContext<{ lang: Lang; setLang: (l: Lang) => void }>({ lang: "en", setLang: () => {} });
export const useLang = () => useContext(Ctx);

/**
 * The language is kept in a cookie so the server layout can also give Clerk's sign-in screens the same language.
 * After a change we refresh the server components so Clerk's localization updates too.
 */
export function LangProvider({ initial, children }: { initial: Lang; children: React.ReactNode }) {
  const [lang, setLangState] = useState<Lang>(initial);
  const router = useRouter();
  useEffect(() => { document.documentElement.lang = lang; }, [lang]);
  const setLang = useCallback((l: Lang) => {
    setLangState(l);
    document.cookie = `sehat_saathi_lang=${l}; Path=/; Max-Age=31536000; SameSite=Lax`;
    document.documentElement.lang = l;
    router.refresh();
  }, [router]);
  return <Ctx.Provider value={{ lang, setLang }}>{children}</Ctx.Provider>;
}
