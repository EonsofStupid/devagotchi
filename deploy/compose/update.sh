#!/usr/bin/env bash
# Deploy the newest image of the tag the hub follows (DEVAGOTCHI_TRACK in .env), pinned by
# digest, and keep it only if the hub answers healthy; otherwise go back to the digest that
# ran before and never try that one again. The systemd timer install.sh writes runs this
# every 5 minutes; `--now` also makes sure the pinned image runs and is healthy.
#
# The deploy files come from GitHub with the image (deploy from GitHub only): a new image's
# `org.opencontainers.image.revision` label names the commit it was built from, and
# compose.yaml, update.sh and backup.sh are fetched from that same commit
# (DEVAGOTCHI_FILES_BASE/<commit>/deploy/compose/, raw.githubusercontent.com by default),
# checked to read with this .env, and swapped in with the image. If the hub is not healthy,
# the files that ran before go back with the image that ran before. An image whose label
# names no commit keeps the files here.
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
files_base="${DEVAGOTCHI_FILES_BASE:-https://raw.githubusercontent.com/$(env_get DEVAGOTCHI_REPO | grep . || echo EonsofStupid/devagotchi)}"
files=(compose.yaml update.sh backup.sh)
[[ -n "$track" ]] || { log "no DEVAGOTCHI_TRACK in $dir/.env"; exit 2; }

healthy() {
  for _ in $(seq 1 45); do
    curl -fsS -m 5 "$health" 2>/dev/null | grep -q '"ok":true' && return 0
    sleep 2
  done
  return 1
}
# The services this stack runs: Postgres and the hub, and Caddy unless an edge elsewhere
# terminates TLS. All of them, so a compose.yaml that changed reaches each.
services() {
  printf '%s\n' postgres hub
  [[ "$(env_get DEVAGOTCHI_EDGE)" == external ]] || printf '%s\n' caddy
}
up() {
  local out
  # shellcheck disable=SC2046
  out=$(docker compose up -d --wait --wait-timeout 120 $(services) 2>&1) || { printf '%s\n' "$out" >&2; return 1; }
  healthy
}
# Put the deploy files in `from` here, each replaced whole (a new file renamed into place,
# so this script, running, keeps reading the one it started with).
swap_files() {
  local from="$1" file
  for file in "${files[@]}"; do
    cp "$from/$file" "./$file.new"
    if [[ "$file" == *.sh ]]; then chmod 0755 "./$file.new"; else chmod 0644 "./$file.new"; fi
    mv "./$file.new" "./$file"
  done
}

# The digest the track points at now.
if [[ "$track" == *@sha256:* ]]; then
  digest="$track"
  docker pull -q "$digest" >/dev/null
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

# The deploy files of the commit the image was built from, staged and checked before
# anything changes.
revision=$(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$digest" 2>/dev/null || true)
staged=""
if [[ "$revision" =~ ^[0-9a-f]{40}$ ]]; then
  staged="$state/files-$revision"
  rm -rf "$staged" && mkdir -p "$staged"
  for file in "${files[@]}"; do
    if ! curl -fsSL -m 60 "$files_base/$revision/deploy/compose/$file" -o "$staged/$file"; then
      log "could not fetch $file of $revision from $files_base; not deploying $digest"; exit 1
    fi
  done
  if ! docker compose -f "$staged/compose.yaml" --env-file .env --project-directory . config -q 2>/dev/null; then
    log "compose.yaml of $revision does not read with this .env; not deploying $digest"; exit 1
  fi
else
  log "$digest names no commit; its deploy files stay as they are"
fi

# A copy of the database before a new image migrates it, and of the deploy files.
if [[ "$current" == *@sha256:* ]] && ! DEVAGOTCHI_BACKUP_DIR="$state/pre-deploy" DEVAGOTCHI_BACKUP_DAYS=7 \
    "$dir/backup.sh" >/dev/null; then
  log "the backup before deploying failed; not deploying $digest"; exit 1
fi
previous="$state/files-previous"
rm -rf "$previous" && mkdir -p "$previous"
cp "${files[@]/#/./}" "$previous/"
was_revision=$(env_get DEVAGOTCHI_REVISION)

log "deploying $digest${staged:+ with the deploy files of $revision} (was $current)"
[[ -n "$staged" ]] && swap_files "$staged"
env_set DEVAGOTCHI_IMAGE "$digest"
[[ -n "$staged" ]] && env_set DEVAGOTCHI_REVISION "$revision"
if up; then
  printf '%s %s %s\n' "$(date -u +%FT%TZ)" "$digest" "${revision:-}" >> "$state/deployed"
  docker image prune -f >/dev/null 2>&1 || true
  log "deployed $digest, healthy"
  exit 0
fi
log "$digest is not healthy; rolling back to $current and the deploy files that ran with it"
printf '%s\n' "$digest" >> "$state/bad"
[[ -n "$staged" ]] && swap_files "$previous"
env_set DEVAGOTCHI_IMAGE "$current"
env_set DEVAGOTCHI_REVISION "$was_revision"
if [[ "$current" == *@sha256:* ]] && up; then
  log "rolled back to $current, healthy"
else
  log "nothing healthy to roll back to: cd $dir && docker compose logs hub"
fi
exit 1
