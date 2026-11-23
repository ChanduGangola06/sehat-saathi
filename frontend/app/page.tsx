"use client";

import AuthScreen from "@/components/AuthScreen";
import Chat from "@/components/Chat";
import { useAuth, useClerk, UserButton } from "@clerk/nextjs";

export default function Home() {
  const { isLoaded, isSignedIn, getToken } = useAuth();
  const { signOut } = useClerk();

  if (!isLoaded) return <div className="auth-wrap"><p className="muted">…</p></div>;
  if (!isSignedIn) return <AuthScreen />;

  return (
    <Chat
      getToken={(skipCache) => getToken({ skipCache })}
      userMenu={<UserButton />}
      // The account is already gone on the server; just end the browser session.
      onDeleted={() => { signOut().catch(() => { }).finally(() => window.location.assign("/")); }}
    />
  );
}
