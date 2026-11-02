#!/usr/bin/env bash
# deploy/compose/install.sh, update.sh and backup.sh for real, as root on a Linux host with
# Docker (CI: `hub (deploy scripts)`): the stack up behind an "external" edge on a loopback
# port, the image pinned by digest, an update that is not healthy rolled back and never
# retried, and a backup of the database and the state volume. Then the same stack re-run
# as a hosted hub (tenancy, an operator, Google from a secrets file), and the deploy files
# coming with each new image from the commit it names (a local registry and file server
# stand in for GHCR and GitHub): swapped in with a healthy image, rolled back with an
# unhealthy one, and not deployed at all when that commit's files cannot be had.
#   sudo bash tests/deploy_scripts.sh
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
work=$(mktemp -d)
dir="$work/hub" port=18790
export DEVAGOTCHI_UPDATE_STATE="$work/update-state" DEVAGOTCHI_DIR="$dir"
registry=dvg-test-registry-$$ files_pid=""
cleanup() {
  (cd "$dir" 2>/dev/null && docker compose down -v >/dev/null 2>&1) || true
  docker rm -f "$registry" >/dev/null 2>&1 || true
  [[ -n "$files_pid" ]] && kill "$files_pid" 2>/dev/null || true
  rm -rf "$work"
}
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
# (Listed whole, not piped to grep -q: an early exit would break tar's pipe under pipefail.)
listed=$(tar -tzf "$work"/backups/state-*.tgz) || fail "the state archive reads back"
[[ -n "$listed" ]] || fail "the state archive has the hub's files"
echo "PASS backup.sh: database dump and state archive"

# A hosted hub: install.sh again with the tenancy, an operator and Sign in with Google, its
# client id and secret from a secrets file (as the secret store writes one).
cat > "$work/hub.env" <<'EOF'
DEVAGOTCHI_GOOGLE_CLIENT_ID=test-client.apps.googleusercontent.com
DEVAGOTCHI_GOOGLE_CLIENT_SECRET='test-secret-not-real'
EOF
out=$("$root/deploy/compose/install.sh" --domain hub.test.example --rp-id test.example \
  --edge external --hub-bind 127.0.0.1 --hub-port "$port" --dir "$dir" --no-timer \
  --tenancy multi --operators email:ops@test.example --google --env-file "$work/hub.env" 2>&1) \
  || { printf '%s\n' "$out" >&2; fail "install.sh re-runs the stack as a hosted hub"; }
[[ "$(stat -c %a "$dir/.env")" == 600 ]] || fail ".env is root's alone"
[[ "$(env_get DEVAGOTCHI_HUB_TENANCY)" == multi ]] || fail ".env names the tenancy"
grep -q "test-secret-not-real" <<<"$out" && fail "install.sh printed a secret"
grep -q "/claim#" <<<"$out" && fail "a hosted hub prints no claim link"
grep -q "nobody claims it" <<<"$out" || fail "install.sh says a hosted hub is not claimed"
curl -fsS "http://127.0.0.1:$port/" | grep -q "Get started" || fail "the hosted landing page gets people started"
[[ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/claim")" == 404 ]] || fail "/claim is a 404 on a hosted hub"
curl -fsS "http://127.0.0.1:$port/auth/login" | grep -q "Continue with Google" || fail "the sign-in page offers Google"
echo "PASS install.sh: a hosted hub with an operator and Google, its secrets kept 0600, no claim"

# The deploy files come with the image, from the commit its label names. A local registry
# holds two images of the running hub, relabelled with made-up commits; a file server holds
# those commits' deploy files: the first's as they are with a marker, the second's with a
# hub that cannot start.
good=$(env_get DEVAGOTCHI_IMAGE)
next_rev=1111111111111111111111111111111111111111 bad_rev=2222222222222222222222222222222222222222
lost_rev=3333333333333333333333333333333333333333
docker run -d --name "$registry" -p 127.0.0.1:5055:5000 \
  registry:2@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373 >/dev/null
for _ in $(seq 1 30); do curl -fsS -o /dev/null http://127.0.0.1:5055/v2/ 2>/dev/null && break; sleep 1; done
for rev in "$next_rev" "$bad_rev" "$lost_rev"; do
  printf 'FROM %s\nLABEL org.opencontainers.image.revision=%s\n' "$good" "$rev" \
    | docker build -q -t "localhost:5055/devagotchi:${rev:0:4}" - >/dev/null
  docker push -q "localhost:5055/devagotchi:${rev:0:4}" >/dev/null
done
for rev in "$next_rev" "$bad_rev"; do
  mkdir -p "$work/files/$rev/deploy/compose"
  cp "$root"/deploy/compose/{compose.yaml,update.sh,backup.sh} "$work/files/$rev/deploy/compose/"
done
printf '# deploy files of %s\n' "$next_rev" >> "$work/files/$next_rev/deploy/compose/compose.yaml"
sed -i 's|"listen": "0.0.0.0:8787"|"listen": "not-an-address"|' "$work/files/$bad_rev/deploy/compose/compose.yaml"
python3 -m http.server 18791 --bind 127.0.0.1 --directory "$work/files" >/dev/null 2>&1 &
files_pid=$!
export DEVAGOTCHI_FILES_BASE=http://127.0.0.1:18791
for _ in $(seq 1 30); do curl -fsS -o /dev/null "$DEVAGOTCHI_FILES_BASE/" 2>/dev/null && break; sleep 1; done
track_to() { sed -i "s|^DEVAGOTCHI_TRACK=.*|DEVAGOTCHI_TRACK=localhost:5055/devagotchi:${1:0:4}|" "$dir/.env"; }

track_to "$next_rev"
"$dir/update.sh" || fail "a healthy image deploys with its commit's files"
grep -q "deploy files of $next_rev" "$dir/compose.yaml" || fail "compose.yaml is the image's commit's"
[[ "$(env_get DEVAGOTCHI_REVISION)" == "$next_rev" ]] || fail ".env names the commit the files came from"
[[ "$(env_get DEVAGOTCHI_IMAGE)" == localhost:5055/devagotchi@sha256:* ]] || fail "the new image is pinned by digest"
curl -fsS "http://127.0.0.1:$port/healthz" | grep -q '"ok":true' || fail "the hub is healthy on the new image and files"
next=$(env_get DEVAGOTCHI_IMAGE)
echo "PASS update.sh: a new image brings the deploy files of its commit"

track_to "$bad_rev"
if "$dir/update.sh"; then fail "files that leave the hub unhealthy are a failure"; fi
grep -q "deploy files of $next_rev" "$dir/compose.yaml" || fail "the files that ran before are back"
grep -q '"listen": "0.0.0.0:8787"' "$dir/compose.yaml" || fail "the broken compose.yaml is gone"
[[ "$(env_get DEVAGOTCHI_IMAGE)" == "$next" ]] || fail "the image that ran before is back"
[[ "$(env_get DEVAGOTCHI_REVISION)" == "$next_rev" ]] || fail "the commit that ran before is back"
curl -fsS "http://127.0.0.1:$port/healthz" | grep -q '"ok":true' || fail "the hub is healthy again"
echo "PASS update.sh: an image whose files leave the hub unhealthy is rolled back with them"

track_to "$lost_rev"
if "$dir/update.sh"; then fail "an image whose commit's files cannot be had is not deployed"; fi
[[ "$(env_get DEVAGOTCHI_IMAGE)" == "$next" ]] || fail "nothing changed for files that could not be fetched"
grep -q "deploy files of $next_rev" "$dir/compose.yaml" || fail "compose.yaml unchanged"
echo "PASS update.sh: no files from the image's commit, no deploy"
echo "DEPLOY SCRIPTS PASSED"
