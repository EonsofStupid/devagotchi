#!/usr/bin/env bash
# Deploy the newest image of the tag the hub follows (DEVAGOTCHI_TRACK in .env), pinned by
# digest, and keep it only if the hub answers healthy; otherwise go back to the digest that
# ran before and never try that one again. The systemd timer install.sh writes runs this
# every 5 minutes; `--now` also makes sure the pinned image runs and is healthy.
set -euo pipefail
dir="${DEVAGOTCHI_DIR:-/opt/devagotchi-hub}"
cd "$dir"
state="${DEVAGOTCHI_UPDATE_STATE:-/var/lib/devagotchi-hub-update}"
mkdir -p "$state" && touch "$state/bad"
log() { printf 'devagotchi-hub-update: %s\n' "$*"; }
env_get() { sed -n "s/^$1=//p" .env | tail -1; }
env_set() {
  awk -v k="$1" -v v="$2" 'BEGIN { done = 0 }
    index($0, k "=") == 1 { print k "=" v; done = 1; next } { print }
    END { if (!done) print k "=" v }' .env > .env.new
  chmod 600 .env.new && mv .env.new .env
}
track=$(env_get DEVAGOTCHI_TRACK)
current=$(env_get DEVAGOTCHI_IMAGE)
bind=$(env_get DEVAGOTCHI_HUB_BIND); [[ -z "$bind" || "$bind" == 0.0.0.0 ]] && bind=127.0.0.1
health="http://$bind:$(env_get DEVAGOTCHI_HUB_PORT | grep . || echo 8787)/healthz"
[[ -n "$track" ]] || { log "no DEVAGOTCHI_TRACK in $dir/.env"; exit 2; }

healthy() {
  for _ in $(seq 1 45); do
    curl -fsS -m 5 "$health" 2>/dev/null | grep -q '"ok":true' && return 0
    sleep 2
  done
  return 1
}
up() {
  local out
  out=$(docker compose up -d --wait --wait-timeout 120 hub 2>&1) || { printf '%s\n' "$out" >&2; return 1; }
  healthy
}

# The digest the track points at now.
if [[ "$track" == *@sha256:* ]]; then
  digest="$track"
else
  docker pull -q "$track" >/dev/null
  repo="${track%:*}"
  digest=$(docker image inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$track" | grep -m1 "^$repo@" || true)
  [[ -n "$digest" ]] || { log "no digest for $track"; exit 1; }
fi

if [[ "$digest" == "$current" ]]; then
  [[ "${1:-}" == --now ]] || exit 0
  if up; then log "running $digest, healthy"; exit 0; fi
  log "$digest runs but is not healthy: cd $dir && docker compose logs hub"; exit 1
fi
if grep -qxF "$digest" "$state/bad"; then
  log "skipping $digest: it failed its health check before"; exit 0
fi

# A copy of the database before a new image migrates it.
if [[ "$current" == *@sha256:* ]] && ! DEVAGOTCHI_BACKUP_DIR="$state/pre-deploy" DEVAGOTCHI_BACKUP_DAYS=7 \
    "$dir/backup.sh" >/dev/null; then
  log "the backup before deploying failed; not deploying $digest"; exit 1
fi
log "deploying $digest (was $current)"
env_set DEVAGOTCHI_IMAGE "$digest"
if up; then
  printf '%s %s\n' "$(date -u +%FT%TZ)" "$digest" >> "$state/deployed"
  docker image prune -f >/dev/null 2>&1 || true
  log "deployed $digest, healthy"
  exit 0
fi
log "$digest is not healthy; rolling back to $current"
printf '%s\n' "$digest" >> "$state/bad"
env_set DEVAGOTCHI_IMAGE "$current"
if [[ "$current" == *@sha256:* ]] && up; then
  log "rolled back to $current, healthy"
else
  log "nothing healthy to roll back to: cd $dir && docker compose logs hub"
fi
exit 1
