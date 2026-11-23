#!/usr/bin/env bash
# Builds a test copy of the frontend, starts the fake OpenAI + fake Clerk + backend, runs the Chrome test.
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
export DB_FILE=/tmp/browser.db; rm -f $DB_FILE
PK="pk_test_$(printf 'example-12.clerk.accounts.dev$' | base64)"
(cd $ROOT/frontend && NEXT_DIST_DIR=.next-e2e NEXT_PUBLIC_E2E=1 NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY=$PK CLERK_SECRET_KEY=sk_test_fake npm run build >/tmp/fe_build.log 2>&1) || { tail -20 /tmp/fe_build.log; exit 1; }
python3 $ROOT/dev/mock_openai.py >/tmp/mock_openai.log 2>&1 & P1=$!
python3 $ROOT/dev/mock_clerk.py serve >/tmp/mock_clerk.log 2>&1 & P2=$!
sleep 1
(cd $ROOT/backend && OPENAI_API_KEY=test OPENAI_BASE_URL=http://127.0.0.1:9999/v1 DATABASE_URL=sqlite://$DB_FILE?mode=rwc \
 CLERK_ISSUER=https://clerk.mock.test CLERK_JWKS_URL=http://127.0.0.1:9998/.well-known/jwks.json CLERK_API_URL=http://127.0.0.1:9998 \
 CLERK_SECRET_KEY=sk_test_mock BILLING_DEV_MODE=true KNOWLEDGE_DIR=$ROOT/backend/knowledge ./target/debug/healthbot >/tmp/backend.log 2>&1) & P3=$!
(cd $ROOT/frontend && NEXT_DIST_DIR=.next-e2e NEXT_PUBLIC_E2E=1 NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY=$PK CLERK_SECRET_KEY=sk_test_fake npx next start -p 3000 >/tmp/fe.log 2>&1) & P4=$!
for i in $(seq 1 60); do curl -sf http://127.0.0.1:8080/api/health >/dev/null && curl -sf -o /dev/null http://localhost:3000/e2e && break; sleep 0.5; done
python3 $ROOT/dev/browser_test.py; RC=$?
pkill -x healthbot; kill $P1 $P2 $P4 2>/dev/null; pkill -f "next start" 2>/dev/null; pkill -f next-server 2>/dev/null; wait 2>/dev/null
rm -rf $ROOT/frontend/.next-e2e
exit $RC
