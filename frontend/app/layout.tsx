import type { Metadata } from "next";
import { Suspense } from "react";
import { ClerkProvider } from "@clerk/nextjs";
import "./globals.css";
import { LangProvider } from "@/lib/lang";
import { cookies } from "next/headers";
import { hiIN } from "@clerk/localizations";

export const metadata: Metadata = {
  title: "Sehat Saathi – AI Health Assistant",
  description: "Chat or talk with an AI health assistant in English or Hindi.",
};

// Reads the language cookie (request data), so it must sit inside <Suspense> with cacheComponents enabled.
// Clerk renders the sign-in / sign-up / profile screens; Hindi texts come from Clerk's community localization.
async function Providers({ children }: { children: React.ReactNode }) {
  const lang = (await cookies()).get("sehat_saathi_lang")?.value === "hi" ? "hi" : "en";
  return (
    <ClerkProvider localization={lang === "hi" ? hiIN : undefined}>
      <LangProvider initial={lang}>{children}</LangProvider>
    </ClerkProvider>
  );
}

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>
        <div className="aurora" aria-hidden><i /><i /><i /></div>
        <Suspense>
          <Providers>{children}</Providers>
        </Suspense>
      </body>
    </html>
  );
}
