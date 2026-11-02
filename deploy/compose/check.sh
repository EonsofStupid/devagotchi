#!/usr/bin/env bash
# Check a running compose stack: the hub answers through Caddy, it is an authorization
# server for AI clients, and its tables are in the stack's Postgres. On a hub nobody has
# claimed yet (always, with --fresh), it also checks the claim: the code from the hub's log
# claims it, as "Owner", without making a passkey, so your own claim afterwards still works
# and names you; and that the owner command works. Run from deploy/compose/ after
# `docker compose up -d`. CI runs it with --fresh (`hub (compose)`).
set -euo pipefail
fresh=false
[[ "${1:-}" == --fresh ]] && fresh=true
cd "$(dirname "$0")"
set -a; . ./.env; set +a
url="${DEVAGOTCHI_PUBLIC_URL:-https://$DEVAGOTCHI_DOMAIN}"
curl_hub() { curl -sS --insecure --max-time 10 "$@"; }
ok() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1" >&2; docker compose logs --tail 60 >&2 || true; exit 1; }

for _ in $(seq 1 90); do
  if curl_hub -o /dev/null -f "$url/healthz" 2>/dev/null; then break; fi
  sleep 2
done
curl_hub -f "$url/healthz" >/dev/null || fail "the hub answers $url/healthz through Caddy"
ok "the hub answers $url/healthz through Caddy"

meta=$(curl_hub -f "$url/.well-known/oauth-authorization-server") || fail "authorization server metadata"
[[ "$meta" == *"\"issuer\":\"$url\""* ]] || fail "the hub is its own authorization server ($meta)"
ok "the hub is its own authorization server for AI clients"

status=$(curl_hub -o /dev/null -w '%{http_code}' -X POST "$url/mcp")
[[ "$status" == 401 ]] || fail "/mcp without a token is refused (got $status)"
ok "/mcp without a token is refused"

tables=$(docker compose exec -T postgres psql -h /var/run/postgresql -U devagotchi -d devagotchi -tAc \
  "SELECT count(*) FROM information_schema.tables WHERE table_name IN ('accounts', 'passkeys', 'oauth_tokens')")
[[ "$tables" == 3 ]] || fail "the hub's tables are in the stack's Postgres (found $tables of 3)"
ok "the hub's tables are in the stack's Postgres"

# Unclaimed (CI never makes a passkey): the hub printed a claim link to its log when it
# started (the newest one works), its landing page offers the claim, and the code from the
# log claims it through Caddy. The browser half is tests/claim_browser.py. Nothing below
# prints the code, but on a failure fail() shows the hub's log, which has it.
home=$(curl_hub -f "$url/") || fail "the landing page answers"
if [[ "$home" != *'href="/claim"'* ]]; then
  $fresh && fail "the landing page of a fresh hub offers the claim"
  ok "the hub is claimed: claim checks skipped"
  exit 0
fi
ok "the landing page of an unclaimed hub offers the claim"

claim=$(docker compose logs --no-color hub 2>/dev/null | grep -o "$url/claim#[0-9A-Z-]*" | tail -1 || true)
[[ -n "$claim" ]] || fail "the hub printed a claim link to its log"
ok "the hub printed a claim link to its log (not shown)"

wrong=$(curl_hub -o /dev/null -w '%{http_code}' -X POST "$url/claim" -H "Origin: $url" \
  -H 'Content-Type: application/json' -d '{"code":"0000-0000-0000-0000-0000-0000"}')
[[ "$wrong" == 403 ]] || fail "a wrong claim code is refused (got $wrong)"
answer=$(curl_hub -X POST "$url/claim" -H "Origin: $url" -H 'Content-Type: application/json' \
  -d "{\"code\":\"${claim#*#}\",\"name\":\"Owner\"}")
[[ "$answer" == *'"link":"dgl_'* && "$answer" == *'"challenge"'* ]] \
  || fail "the code from the log claims the hub and asks for the owner's passkey"
ok "the code from the log claims the hub and asks for the owner's passkey"

link=$(docker compose exec -T hub devagotchi hub owner --state-dir /var/lib/devagotchi --config /etc/devagotchi/hub.json 2>/dev/null)
[[ "$link" == "$url/owner#dgl_"* ]] || fail "the owner command prints a one-time link"
ok "the owner command still prints a one-time link (not shown)"

owners=$(docker compose exec -T postgres psql -h /var/run/postgresql -U devagotchi -d devagotchi -tAc \
  "SELECT count(*) FROM accounts WHERE owner = 1")
[[ "$owners" == 1 ]] || fail "the owner's account is in Postgres (found $owners)"
codes=$(docker compose exec -T postgres psql -h /var/run/postgresql -U devagotchi -d devagotchi -tAc \
  "SELECT count(*) FROM claim_codes")
[[ "$codes" == 1 ]] || fail "one claim code, as a hash, is in Postgres (found $codes)"
ok "the owner's account and one hashed claim code are in Postgres"
