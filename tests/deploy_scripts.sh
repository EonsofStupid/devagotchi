#!/usr/bin/env bash
# deploy/compose/install.sh, update.sh and backup.sh for real, as root on a Linux host with
# Docker (CI: `hub (deploy scripts)`): the stack up behind an "external" edge on a loopback
# port, the image pinned by digest, an update that is not healthy rolled back and never
# retried, and a backup of the database and the state volume.
#   sudo bash tests/deploy_scripts.sh
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
work=$(mktemp -d)
dir="$work/hub" port=18790
export DEVAGOTCHI_UPDATE_STATE="$work/update-state" DEVAGOTCHI_DIR="$dir"
cleanup() { (cd "$dir" 2>/dev/null && docker compose down -v >/dev/null 2>&1) || true; rm -rf "$work"; }
trap cleanup EXIT
fail() { printf 'FAIL %s\n' "$1" >&2; (cd "$dir" && docker compose logs --tail 40) >&2 || true; exit 1; }
env_get() { sed -n "s/^$1=//p" "$dir/.env" | tail -1; }

out=$("$root/deploy/compose/install.sh" --domain hub.test.example --rp-id test.example \
  --edge external --hub-bind 127.0.0.1 --hub-port "$port" --dir "$dir" --no-timer 2>&1) \
  || { printf '%s\n' "$out" >&2; fail "install.sh brings the stack up"; }
[[ "$(env_get DEVAGOTCHI_IMAGE)" == ghcr.io/eonsofstupid/devagotchi@sha256:* ]] || fail "the image is pinned by digest"
curl -fsS "http://127.0.0.1:$port/healthz" | grep -q '"ok":true' || fail "the hub answers healthy on its port"
grep -q "https://hub.test.example/claim#" <<<"$out" || fail "install.sh prints the claim link"
echo "PASS install.sh: stack up, image pinned by digest, claim link printed"

good=$(env_get DEVAGOTCHI_IMAGE)
"$dir/update.sh" || fail "an update with nothing new is a no-op"
[[ "$(env_get DEVAGOTCHI_IMAGE)" == "$good" ]] || fail "nothing new changes nothing"
echo "PASS update.sh: nothing new, nothing changed"

# An image that is not a hub at all: deployed, found unhealthy, rolled back, remembered.
bad=caddy:2@sha256:36c24ad91a7ff7bc4c87fe22bfa8df3c4b18da0773d73fc12ee2c52b98c0bb7e
sed -i "s|^DEVAGOTCHI_TRACK=.*|DEVAGOTCHI_TRACK=$bad|" "$dir/.env"
if "$dir/update.sh"; then fail "an unhealthy image is reported as a failure"; fi
[[ "$(env_get DEVAGOTCHI_IMAGE)" == "$good" ]] || fail "it rolls back to the digest that ran"
curl -fsS "http://127.0.0.1:$port/healthz" | grep -q '"ok":true' || fail "the hub is healthy again"
ls "$DEVAGOTCHI_UPDATE_STATE"/pre-deploy/db-*.dump >/dev/null || fail "the database is backed up before a deploy"
"$dir/update.sh" | grep -q "skipping $bad" || fail "a digest that failed is not tried again"
echo "PASS update.sh: an unhealthy image is rolled back and not retried"

DEVAGOTCHI_BACKUP_DIR="$work/backups" "$dir/backup.sh" >/dev/null || fail "backup.sh runs"
ls "$work"/backups/db-*.dump "$work"/backups/state-*.tgz >/dev/null || fail "backup.sh writes both files"
tar -tzf "$work"/backups/state-*.tgz | grep -q . || fail "the state archive has the hub's files"
echo "PASS backup.sh: database dump and state archive"
echo "DEPLOY SCRIPTS PASSED"
