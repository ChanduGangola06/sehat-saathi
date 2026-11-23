import { clerkMiddleware } from "@clerk/nextjs/server";
import { NextResponse } from "next/server";

// Clerk's middleware keeps the session cookie fresh. Routes are not protected here: the page shows the sign-in
// screen to signed-out visitors and our API checks the token on every request.
// (NEXT_PUBLIC_E2E is only set by the offline browser test in dev/, never in a real deployment.)
export default process.env.NEXT_PUBLIC_E2E === "1" ? () => NextResponse.next() : clerkMiddleware();

export const config = {
  matcher: [
    // Skip Next.js internals and static files, always run for API routes.
    "/((?!_next|[^?]*\\.(?:html?|css|js(?!on)|jpe?g|webp|png|gif|svg|ttf|woff2?|ico|csv|docx?|xlsx?|zip|webmanifest)).*)",
    "/(api|trpc)(.*)",
  ],
};
