#!/usr/bin/env bash
# End-to-end test: Clerk token verification, daily token quota by timezone, billing, chat features, webhook, deletion.
# Easiest way to run it: dev/run_e2e.sh (starts the fake OpenAI, fake Clerk and the backend for you).
set -uo pipefail
B=${BACKEND:-http://127.0.0.1:8080}
DIR=$(cd "$(dirname "$0")" && pwd)
MC="${PYTHON:-python3} $DIR/mock_clerk.py"
DB=${DB_FILE:-/tmp/e2e.db}
WHSEC=${CLERK_WEBHOOK_SECRET:?set CLERK_WEBHOOK_SECRET}
PASS=0; FAIL=0
U1="user_e2e_$RANDOM"; U2="user_e2e_$RANDOM"; U3="user_e2e_$RANDOM"
j() { curl -sS -H 'Content-Type: application/json' "$@"; }
code() { curl -sS -o /dev/null -w '%{http_code}' -H 'Content-Type: application/json' "$@"; }
field() { python3 -c "import sys,json;d=json.load(sys.stdin);print(eval('d'+sys.argv[1]))" "$1" 2>/dev/null; }
check() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "  ok   $1"; else FAIL=$((FAIL+1)); echo "  FAIL $1 (expected '$3', got '$2')"; fi; }
tok() { $MC token "$@"; }
A() { echo "Authorization: Bearer $TOKEN"; }
hook() { # hook '<json>' [--age N]  -> HTTP status
  read -r id ts sig < <($MC svix "$WHSEC" "$1" "${@:2}")
  code -H "svix-id: $id" -H "svix-timestamp: $ts" -H "svix-signature: $sig" -d "$1" $B/api/clerk/webhook
}

echo "== Clerk session tokens"
TOKEN=$(tok $U1)
# Cold start: many requests arrive at once, before the signing keys have been fetched. All must succeed.
rm -f /tmp/par_*.txt
for i in 1 2 3 4 5 6 7 8; do (code -H "$(A)" -H 'X-Timezone: Asia/Tokyo' $B/api/me > /tmp/par_$i.txt; echo >> /tmp/par_$i.txt) & done; wait
check "8 simultaneous first requests all succeed" "$(cat /tmp/par_*.txt | grep -c '^200$')" 8
check "no token -> 401"                "$(code $B/api/me)" 401
check "garbage token -> 401"           "$(code -H 'Authorization: Bearer abc.def.ghi' $B/api/me)" 401
check "expired token -> 401"           "$(code -H "Authorization: Bearer $(tok $U1 --expired)" $B/api/me)" 401
check "token for another site (azp) -> 401" "$(code -H "Authorization: Bearer $(tok $U1 --azp https://evil.example)" $B/api/me)" 401
check "unknown signing key -> 401"     "$(code -H "Authorization: Bearer $(tok $U1 --kid not-a-real-key)" $B/api/me)" 401
check "tampered signature -> 401"      "$(code -H "Authorization: Bearer ${TOKEN}x" $B/api/me)" 401
check "chat without token -> 401"      "$(code -d '{"message":"hi"}' $B/api/chat)" 401
check "valid token accepted"           "$(curl -sS -H "$(A)" -H 'X-Timezone: Asia/Tokyo' $B/api/me | field "['user']['id']")" "$U1"
check "starts on the free plan"        "$(curl -sS -H "$(A)" $B/api/me | field "['subscription']['plan']['id']")" free

echo "== daily token allowance"
S=$(curl -sS -H "$(A)" $B/api/billing/status)
check "free limit is per day"          "$(echo "$S" | field "['limit']")" 2000
check "timezone taken from browser header" "$(echo "$S" | field "['timezone']")" Asia/Tokyo
python3 - "$S" <<'PY' || exit 1
import json, sys, time
from zoneinfo import ZoneInfo
from datetime import datetime, timedelta
s = json.loads(sys.argv[1]); tz = ZoneInfo("Asia/Tokyo")
now = datetime.now(tz); nxt = (now + timedelta(days=1)).replace(hour=0, minute=0, second=0, microsecond=0)
assert abs(s["resets_at"] - int(nxt.timestamp())) <= 1, (s["resets_at"], int(nxt.timestamp()))
print("  ok   reset time is the next midnight in Tokyo")
PY
PASS=$((PASS+1))
check "timezone change #1 allowed"     "$(code -X PUT -H "$(A)" -d '{"timezone":"Asia/Kolkata"}' $B/api/timezone)" 200
check "timezone change #2 blocked (30-day cooldown)" "$(code -X PUT -H "$(A)" -d '{"timezone":"America/New_York"}' $B/api/timezone)" 429
check "unknown timezone rejected"      "$(code -X PUT -H "$(A)" -d '{"timezone":"Mars/Base"}' $B/api/timezone)" 400
check "timezone now India"             "$(curl -sS -H "$(A)" $B/api/me | field "['subscription']['timezone']")" Asia/Kolkata

echo "== chat, tools, quota"
TOKEN=$(tok $U2)
check "emergency answered"             "$(j -H "$(A)" -d '{"message":"severe chest pain"}' $B/api/chat | field "['emergency']")" True
check "vital logged via agent"         "$(j -H "$(A)" -d '{"message":"my BP is 130/85"}' $B/api/chat | field "['tools_used'][0]")" log_vital
check "vital visible"                  "$(curl -sS -H "$(A)" $B/api/vitals | field "['vitals'][0]['value']")" 130/85
check "usage is being counted"         "$([ "$(curl -sS -H "$(A)" $B/api/billing/status | field "['used']")" -gt 0 ] && echo yes)" yes
j -H "$(A)" -d '{"message":"please remind me"}' $B/api/chat >/dev/null
check "daily quota exhausted -> 402"   "$(code -H "$(A)" -d '{"message":"I have a headache"}' $B/api/chat)" 402
check "emergency still answered at 0 tokens" "$(j -H "$(A)" -d '{"message":"I can not breathe"}' $B/api/chat | field "['emergency']")" True
check "402 carries quota_exceeded"     "$(j -H "$(A)" -d '{"message":"I have a headache"}' $B/api/chat | field "['code']")" quota_exceeded
check "remaining is 0"                 "$(curl -sS -H "$(A)" $B/api/billing/status | field "['remaining']")" 0
echo "  ...simulating the user's local midnight passing (moving today's usage to yesterday)"
python3 -c "import sqlite3;c=sqlite3.connect('$DB');c.execute('UPDATE usage SET created_at=created_at-172800 WHERE user_id=?',('$U2',));c.commit()"
check "allowance is back after midnight" "$(curl -sS -H "$(A)" $B/api/billing/status | field "['remaining']")" 2000
check "chat works again next day"      "$(code -H "$(A)" -d '{"message":"I have a headache"}' $B/api/chat)" 200

echo "== billing"
check "plans are public"               "$(curl -sS $B/api/billing/plans | field "['plans'][1]['id']")" plus
check "plans expose daily limits"      "$(curl -sS $B/api/billing/plans | field "['plans'][1]['daily_tokens']")" 2000
check "cannot checkout free plan"      "$(code -H "$(A)" -d '{"plan_id":"free"}' $B/api/billing/checkout)" 400
O=$(j -H "$(A)" -d '{"plan_id":"plus"}' $B/api/billing/checkout | field "['order_id']")
check "forged payment webhook rejected" "$(code -H 'X-Signature: 00' -d "{\"order_id\":\"$O\",\"status\":\"paid\"}" $B/api/billing/webhook)" 401
check "still free after forgery"       "$(curl -sS -H "$(A)" $B/api/billing/status | field "['plan']['id']")" free
BODY="{\"order_id\":\"$O\",\"status\":\"paid\",\"provider_ref\":\"pay_123\"}"
SIG=$(python3 -c "import hmac,hashlib,sys;print(hmac.new(b'whsec_test_secret_123456',sys.argv[1].encode(),hashlib.sha256).hexdigest())" "$BODY")
check "signed payment webhook accepted" "$(code -H "X-Signature: $SIG" -d "$BODY" $B/api/billing/webhook)" 200
check "upgraded to plus"               "$(curl -sS -H "$(A)" $B/api/billing/status | field "['plan']['id']")" plus
check "plus limit is 2000/day"        "$(curl -sS -H "$(A)" $B/api/billing/status | field "['limit']")" 2000
check "replayed webhook harmless"      "$(code -H "X-Signature: $SIG" -d "$BODY" $B/api/billing/webhook)" 200
O2=$(j -H "$(A)" -d '{"plan_id":"pro"}' $B/api/billing/checkout | field "['order_id']")
check "dev-activate (dev mode)"        "$(code -H "$(A)" -d "{\"order_id\":\"$O2\"}" $B/api/billing/dev-activate)" 200
check "now on pro"                     "$(curl -sS -H "$(A)" $B/api/billing/status | field "['plan']['id']")" pro

echo "== data isolation and documents"
TOKEN2=$TOKEN; TOKEN=$(tok $U3)
check "other user sees no history"     "$(curl -sS -H "$(A)" $B/api/history | field "['messages'].__len__()")" 0
check "other user sees no vitals"      "$(curl -sS -H "$(A)" $B/api/vitals | field "['vitals'].__len__()")" 0
check "other user can't touch reminder" "$(code -X POST -H "$(A)" $B/api/reminders/1/done)" 404
TOKEN=$TOKEN2
check "documents upload"               "$(code -H "$(A)" -d '{"name":"lab.txt","text":"Hemoglobin 11.2 g/dL low"}' $B/api/documents)" 200

echo "== rate limit"
TOKEN=$(tok user_rl_$RANDOM); N=0
for i in $(seq 1 25); do [ "$(code -H "$(A)" -d '{"message":"severe chest pain"}' $B/api/chat)" = 429 ] && N=$((N+1)); done
check "per-user request cap kicks in (20/min)" "$([ $N -ge 1 ] && echo yes)" yes

echo "== Clerk webhook (Svix) and account deletion"
EV="{\"type\":\"user.deleted\",\"data\":{\"id\":\"$U2\",\"deleted\":true,\"object\":\"user\"}}"
check "unsigned webhook rejected"      "$(code -d "$EV" $B/api/clerk/webhook)" 401
check "bad signature rejected"         "$(code -H 'svix-id: msg_x' -H "svix-timestamp: $(date +%s)" -H 'svix-signature: v1,AAAA' -d "$EV" $B/api/clerk/webhook)" 401
check "stale (replayed) webhook rejected" "$(hook "$EV" --age 3600)" 401
TOKEN=$TOKEN2
check "U2 has data before erasure"     "$([ "$(curl -sS -H "$(A)" $B/api/history | field "['messages'].__len__()")" -gt 0 ] && echo yes)" yes
check "valid user.deleted accepted"    "$(hook "$EV")" 200
python3 -c "import sqlite3;c=sqlite3.connect('$DB');print(sum(c.execute(f'SELECT COUNT(*) FROM {t} WHERE user_id=?',('$U2',)).fetchone()[0] for t in ['messages','vitals','reminders','chunks','usage','subscriptions'])+c.execute('SELECT COUNT(*) FROM users WHERE id=?',('$U2',)).fetchone()[0])" > /tmp/left.txt
check "all U2 personal data erased"    "$(cat /tmp/left.txt)" 0
check "other webhook events ignored"   "$(hook '{"type":"user.created","data":{"id":"user_zzz"}}')" 200
TOKEN=$(tok $U1)
check "DELETE /api/account works"      "$(code -X DELETE -H "$(A)" $B/api/account)" 200
check "Clerk user was deleted via Backend API" "$(grep -c "^$U1\$" /tmp/mock_clerk_deleted.txt)" 1
python3 -c "import sqlite3;c=sqlite3.connect('$DB');print(c.execute('SELECT COUNT(*) FROM users WHERE id=?',('$U1',)).fetchone()[0])" > /tmp/left.txt
check "local profile erased"           "$(cat /tmp/left.txt)" 0

echo; echo "passed: $PASS   failed: $FAIL"; [ $FAIL -eq 0 ]
