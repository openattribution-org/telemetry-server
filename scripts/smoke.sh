#!/usr/bin/env bash
#
# End-to-end check against a running build: ingest, validation, ctx-token
# disclosure, and session-document materialisation, over real HTTP against a
# real database.
#
# Usage:
#   DATABASE_URL=postgres://user:pass@localhost/ctref ./scripts/smoke.sh
#
# The database must exist and be empty or already migrated; the server applies
# its own migrations on boot. Set PSQL if psql is not on your PATH — e.g. for
# a containerised Postgres:
#
#   PSQL="docker exec -i my-pg psql -U postgres -d ctref" ./scripts/smoke.sh

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

: "${DATABASE_URL:?set DATABASE_URL to a PostgreSQL database this script may write to}"
PSQL="${PSQL:-psql "$DATABASE_URL"}"
BIND_ADDR="${BIND_ADDR:-127.0.0.1:8099}"
BASE="http://$BIND_ADDR"
BIN="${BIN:-./target/debug/content-telemetry-server}"

export DATABASE_URL BIND_ADDR
export RUST_LOG="${RUST_LOG:-warn}"

AGENT='11111111-1111-1111-1111-111111111111'
PUBLISHER='22222222-2222-2222-2222-222222222222'
URL='https://example.com/article-1'

FAILURES=0
ok()   { echo "  PASS  $1"; }
bad()  { echo "  FAIL  $1"; FAILURES=$((FAILURES+1)); }
jget() { python3 -c "import sys,json;d=json.load(sys.stdin);print($1)"; }
uuid() { python3 -c 'import uuid;print(uuid.uuid4())'; }
now()  { date -u +%Y-%m-%dT%H:%M:%SZ; }

[ -x "$BIN" ] || { echo "no binary at $BIN — run 'cargo build' first"; exit 1; }

"$BIN" >/tmp/content-telemetry-smoke.log 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null' EXIT

for _ in $(seq 1 40); do
  curl -sf "$BASE/ready" >/dev/null 2>&1 && break
  sleep 0.25
done

echo "== migrations + readiness"
curl -sf "$BASE/ready" >/dev/null \
  && ok "server ready, migrations applied" \
  || { bad "server never became ready"; cat /tmp/content-telemetry-smoke.log; exit 1; }

$PSQL -q < examples/dev-seed.sql >/dev/null 2>&1 \
  && ok "dev seed loaded" || bad "dev seed failed"

echo "== auth seam"
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/sessions/start" \
  -H 'content-type: application/json' -d '{"initiator_type":"user"}')
[ "$CODE" = "401" ] && ok "request without org header rejected (401)" || bad "expected 401, got $CODE"

echo "== session lifecycle"
SESSION=$(curl -s -X POST "$BASE/sessions/start" \
  -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d '{"initiator_type":"user","agent_id":"demo-agent","conformance_level":"citation"}' \
  | jget 'd["session_id"]')
[ -n "$SESSION" ] && ok "session started" || bad "session start failed"

CITED=$(uuid); PRESENTED=$(uuid); TS=$(now)

RESP=$(curl -s -X POST "$BASE/events" \
  -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d "{
    \"document_type\":\"event_batch\",
    \"schema_version\":\"1.0\",
    \"session_id\":\"$SESSION\",
    \"events\":[
      {\"type\":\"content_grounded\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\",\"data\":{\"scope\":\"session\"}},
      {\"id\":\"$CITED\",\"type\":\"content_cited\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\",\"output_id\":\"out-1\",\"data\":{\"citation_type\":\"direct_quote\"}},
      {\"id\":\"$PRESENTED\",\"type\":\"content_presented\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\",\"output_id\":\"out-1\",\"citation_id\":\"$CITED\",\"data\":{\"presentation_kind\":\"content\",\"presentation_type\":\"summary\"}}
    ]}")
[ "$(echo "$RESP" | jget 'd.get("events_created","ERR")')" = "3" ] \
  && ok "3 events ingested" || bad "expected 3 events, got: $RESP"

echo "== v1 structural rules enforced"
RESP=$(curl -s -X POST "$BASE/events" -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d "{\"session_id\":\"$SESSION\",\"events\":[{\"id\":\"$(uuid)\",\"type\":\"content_cited\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\"}]}")
echo "$RESP" | grep -q 'output_id' && ok "content_cited without output_id rejected" || bad "expected output_id error, got: $RESP"

RESP=$(curl -s -X POST "$BASE/events" -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d "{\"session_id\":\"$SESSION\",\"events\":[{\"type\":\"content_engaged\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\"}]}")
echo "$RESP" | grep -q 'presentation_id' && ok "content_engaged without presentation_id rejected" || bad "expected presentation_id error, got: $RESP"

RESP=$(curl -s -X POST "$BASE/events" -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d "{\"session_id\":\"$SESSION\",\"events\":[{\"type\":\"content_displayed\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\"}]}")
echo "$RESP" | grep -q 'withdrawn' && ok "withdrawn content_displayed rejected" || bad "expected withdrawn error, got: $RESP"

RESP=$(curl -s -X POST "$BASE/events" -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d "{\"schema_version\":\"9.9\",\"session_id\":\"$SESSION\",\"events\":[{\"type\":\"content_grounded\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\"}]}")
echo "$RESP" | grep -q 'unsupported schema_version' && ok "unknown schema_version rejected" || bad "expected version error, got: $RESP"

RESP=$(curl -s -X POST "$BASE/events" -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d "{\"events\":[{\"type\":\"content_retrieved\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\",\"source_role\":\"agent\"}]}")
echo "$RESP" | grep -q 'session_id or ctx_token' && ok "agent role cannot emit sessionless" || bad "expected sessionless error, got: $RESP"

echo "== sessionless retrieval (edge emitter)"
RESP=$(curl -s -X POST "$BASE/events" -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d "{\"events\":[{\"type\":\"content_retrieved\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\",\"source_role\":\"edge\",\"content_telemetry_id\":\"$(uuid)\"}]}")
[ "$(echo "$RESP" | jget 'd.get("events_created","ERR")')" = "1" ] \
  && ok "edge retrieval accepted without a session" || bad "sessionless ingest failed: $RESP"

echo "== ctx token + two-sided consent"
TOKEN=$(curl -s -X POST "$BASE/click-tokens" -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d "{\"session_id\":\"$SESSION\",\"content_url\":\"$URL\"}" | jget 'd["token"]')
[ -n "$TOKEN" ] && ok "ctx token minted" || bad "ctx token creation failed"

MANIFEST=$(curl -s "$BASE/ctx/$TOKEN")
echo "$MANIFEST" | grep -q "$URL" && ok "manifest discloses consenting publisher's URL" || bad "manifest missing URL: $MANIFEST"
echo "$MANIFEST" | grep -q "$SESSION" && bad "manifest leaked the session id" || ok "manifest withholds the session id"

CODE=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/ctx/does-not-exist")
[ "$CODE" = "404" ] && ok "unknown ctx token is 404" || bad "expected 404, got $CODE"

echo "== engagement reported through the ctx token"
RESP=$(curl -s -X POST "$BASE/events" -H 'content-type: application/json' -H "x-organization-id: $PUBLISHER" \
  -d "{\"ctx_token\":\"$TOKEN\",\"events\":[{\"type\":\"content_engaged\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\",\"presentation_id\":\"$PRESENTED\"}]}")
[ "$(echo "$RESP" | jget 'd.get("events_created","ERR")')" = "1" ] \
  && ok "engagement bound via ctx token" || bad "ctx-bound ingest failed: $RESP"

RESP=$(curl -s -X POST "$BASE/events" -H 'content-type: application/json' -H "x-organization-id: $PUBLISHER" \
  -d "{\"ctx_token\":\"$TOKEN\",\"events\":[{\"id\":\"$(uuid)\",\"type\":\"content_cited\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\",\"output_id\":\"out-9\"}]}")
echo "$RESP" | grep -q 'content_engaged' && ok "ctx token cannot write non-engagement claims" || bad "expected ctx restriction, got: $RESP"

echo "== standard session document"
DOC=$(curl -s "$BASE/sessions/$SESSION/document" -H "x-organization-id: $AGENT")
[ "$(echo "$DOC" | jget 'd["document_type"]')" = "session" ] && ok "document_type is session" || bad "bad document: $DOC"
[ "$(echo "$DOC" | jget 'd["conformance_level"]')" = "citation" ] && ok "conformance level preserved" || bad "conformance level wrong"
[ "$(echo "$DOC" | jget 'len(d["events"])')" -ge 3 ] && ok "document carries its events" || bad "document events missing"

echo "== end session"
RESP=$(curl -s -X POST "$BASE/sessions/end" -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d "{\"session_id\":\"$SESSION\",\"outcome\":{\"type\":\"conversion\",\"value_amount\":2500,\"currency\":\"GBP\"}}")
[ "$(echo "$RESP" | jget 'd.get("status","ERR")')" = "ok" ] && ok "session ended with outcome" || bad "end failed: $RESP"

RESP=$(curl -s -X POST "$BASE/events" -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d "{\"session_id\":\"$SESSION\",\"events\":[{\"type\":\"content_grounded\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\",\"data\":{\"scope\":\"session\"}}]}")
echo "$RESP" | grep -q 'session has ended' && ok "events rejected after session end" || bad "expected ended-session error, got: $RESP"

echo "== bulk session document"
RESP=$(curl -s -X POST "$BASE/sessions/bulk" -H 'content-type: application/json' -H "x-organization-id: $AGENT" \
  -d "{\"document_type\":\"session\",\"schema_version\":\"1.0\",\"session_id\":\"$(uuid)\",\"agent_id\":\"demo-agent\",
       \"events\":[{\"type\":\"content_grounded\",\"timestamp\":\"$TS\",\"content_url\":\"$URL\",\"data\":{\"scope\":\"session\"}}],
       \"outcome\":{\"type\":\"browse\"}}")
[ "$(echo "$RESP" | jget 'd.get("events_created","ERR")')" = "1" ] && ok "bulk document ingested" || bad "bulk failed: $RESP"
[ "$(echo "$RESP" | jget 'str(d.get("outcome_recorded"))')" = "True" ] && ok "bulk outcome recorded" || bad "bulk outcome not recorded: $RESP"

echo
if [ "$FAILURES" -eq 0 ]; then echo "ALL CHECKS PASSED"; else echo "$FAILURES CHECK(S) FAILED"; fi
exit "$FAILURES"
