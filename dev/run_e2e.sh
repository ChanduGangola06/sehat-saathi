#!/usr/bin/env bash
# Starts the fake OpenAI + fake Clerk + backend, runs dev/e2e.sh, cleans up.
# Needs `pip install pyjwt cryptography` (set PYTHON=/path/to/python if they live in a venv) and `cargo build` first.
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
export DB_FILE=/tmp/e2e.db; rm -f $DB_FILE
export CLERK_WEBHOOK_SECRET="whsec_$(python3 -c 'import base64;print(base64.b64encode(b"k"*24).decode())')"
${PYTHON:-python3} $ROOT/dev/mock_openai.py >/tmp/mock_openai.log 2>&1 & P1=$!
${PYTHON:-python3} $ROOT/dev/mock_clerk.py serve >/tmp/mock_clerk.log 2>&1 & P2=$!
sleep 1
cd $ROOT/backend
AI_PROVIDERS=openai GEMINI_API_KEY= OPENAI_API_KEY=test OPENAI_BASE_URL=http://127.0.0.1:9999/v1 DATABASE_URL=sqlite://$DB_FILE?mode=rwc \
CLERK_ISSUER=https://clerk.mock.test CLERK_JWKS_URL=http://127.0.0.1:9998/.well-known/jwks.json \
CLERK_API_URL=http://127.0.0.1:9998 CLERK_SECRET_KEY=sk_test_mock \
FREE_DAILY_TOKENS=2000 BILLING_DEV_MODE=true BILLING_WEBHOOK_SECRET=whsec_test_secret_123456 \
KNOWLEDGE_DIR=$ROOT/backend/knowledge ./target/debug/sehat-saathi >/tmp/backend.log 2>&1 & P3=$!
for i in $(seq 1 40); do curl -sf http://127.0.0.1:8080/api/health >/dev/null && break; sleep 0.5; done
bash $ROOT/dev/e2e.sh; RC=$?
kill $P1 $P2 $P3 2>/dev/null; wait 2>/dev/null
exit $RC
