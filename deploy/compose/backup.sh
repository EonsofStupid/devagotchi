#!/usr/bin/env bash
# Back up the hub: its database (pg_dump, custom format) and its state volume (keys,
# machine enrolments, audit), kept for DEVAGOTCHI_BACKUP_DAYS (14). install.sh's timer runs
# it nightly. Restore: pg_restore --clean into the stack's Postgres, and untar the state
# into the hub-state volume, with the hub stopped. Copy the directory off this host too.
set -euo pipefail
dir="${DEVAGOTCHI_DIR:-/opt/devagotchi-hub}"
out="${DEVAGOTCHI_BACKUP_DIR:-/var/backups/devagotchi-hub}"
days="${DEVAGOTCHI_BACKUP_DAYS:-14}"
cd "$dir"
install -d -m 0700 "$out"
ts=$(date -u +%Y%m%dT%H%M%SZ)
docker compose exec -T postgres pg_dump -h /var/run/postgresql -U devagotchi --format=custom devagotchi \
  > "$out/db-$ts.dump.part"
mv "$out/db-$ts.dump.part" "$out/db-$ts.dump"
pg=$(docker compose ps -q postgres)
project=$(docker inspect --format '{{index .Config.Labels "com.docker.compose.project"}}' "$pg")
volume=$(docker volume ls -q --filter "label=com.docker.compose.project=$project" --filter "label=com.docker.compose.volume=hub-state" | head -1)
[[ -n "$volume" ]] || { echo "devagotchi-hub-backup: no hub-state volume for project $project" >&2; exit 1; }
image=$(docker inspect --format '{{.Image}}' "$pg")
docker run --rm --network none -v "$volume:/state:ro" "$image" tar -C /state -czf - . > "$out/state-$ts.tgz.part"
mv "$out/state-$ts.tgz.part" "$out/state-$ts.tgz"
find "$out" -maxdepth 1 \( -name 'db-*.dump' -o -name 'state-*.tgz' \) -mtime +"$days" -delete
echo "devagotchi-hub-backup: $out/db-$ts.dump, $out/state-$ts.tgz"
