#!/usr/bin/env bash
# Reload a Proxmox VE container as the Devagotchi hub: the hub image from GHCR, its own
# Postgres, deployed again by itself whenever a new image lands. Run it as root on the
# Proxmox host:
#
#   curl -fsSL https://raw.githubusercontent.com/EonsofStupid/devagotchi/main/deploy/proxmox/hub-up.sh \
#     | bash -s -- --ct 110 --domain hub.example.com
#
# 1. Snapshots the container (devagotchi-<time>), so `pct rollback` undoes all of this.
# 2. Turns on what Docker needs in it (nesting; keyctl when unprivileged), restarting it
#    if those were off.
# 3. Stops and disables a hub installed there before (`devagotchi-hub`, user or system unit).
# 4. Runs deploy/compose/install.sh inside it from one commit: Docker, the image pinned by
#    digest, Postgres, a 5-minute timer that deploys each new image and rolls back one that
#    is not healthy, and a nightly backup. When a Caddy in the container already serves the
#    domain, the hub takes its old port on loopback and that Caddy keeps the certificates;
#    otherwise the stack's own Caddy takes ports 80 and 443.
# 5. Checks https://<domain> answers from the new hub, and prints the claim link: open it on
#    the phone that will hold the owner's passkey.
#
# Re-running it is safe. Nothing here names a host, a domain or a container of its own.
set -euo pipefail

usage() {
  cat <<'EOF'
hub-up.sh --ct ID --domain NAME [options]
  --ct ID            the container to reload as the hub (required)
  --domain NAME      the name the hub answers at (required)
  --rp-id NAME       passkeys' relying party: NAME or a parent domain (default NAME)
  --port PORT        the hub's port on the container's loopback (default 8787)
  --track IMAGE      the image tag the hub follows (default ghcr.io/eonsofstupid/devagotchi:main)
  --repo OWNER/NAME --ref REF
                     where the deploy files come from (default EonsofStupid/devagotchi at
                     main, resolved to one commit)
  --no-snapshot      do not snapshot the container first
EOF
}

ct="" domain="" rp_id="" port=8787 track=ghcr.io/eonsofstupid/devagotchi:main
repo=EonsofStupid/devagotchi ref=main snapshot=true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --ct) ct="$2"; shift 2 ;;
    --domain) domain="$2"; shift 2 ;;
    --rp-id) rp_id="$2"; shift 2 ;;
    --port) port="$2"; shift 2 ;;
    --track) track="$2"; shift 2 ;;
    --repo) repo="$2"; shift 2 ;;
    --ref) ref="$2"; shift 2 ;;
    --no-snapshot) snapshot=false; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "hub-up.sh: unknown option $1" >&2; usage >&2; exit 2 ;;
  esac
done

# For tests (tests/proxmox_hub_up.py): another log file, and no root.
log_file="${DEVAGOTCHI_HUB_UP_LOG:-/var/log/devagotchi-hub-up.log}"
exec > >(tee -a "$log_file") 2>&1
say() { printf '\n==> %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }
die() { printf '\nhub-up.sh: %s\n(log: %s)\n' "$*" "$log_file" >&2; exit 1; }

[[ -n "$ct" && -n "$domain" ]] || { usage >&2; exit 2; }
[[ "$ct" =~ ^[0-9]+$ ]] || die "--ct $ct is not a container id"
[[ "$domain" =~ ^[A-Za-z0-9.-]+$ ]] || die "--domain $domain is not a host name"
[[ "$port" =~ ^[0-9]+$ ]] || die "--port is a number"
[[ $EUID -eq 0 || -n "${DEVAGOTCHI_ALLOW_NONROOT:-}" ]] || die "run it as root on the Proxmox host"
for tool in pct curl; do
  command -v "$tool" >/dev/null || die "$tool is missing: this runs on a Proxmox VE host"
done
domain="${domain,,}"; rp_id="${rp_id,,}"
rp_id="${rp_id:-$domain}"
in_ct() { pct exec "$ct" -- "$@"; }
config=$(pct config "$ct" 2>/dev/null) || die "there is no container $ct on this host"

# 1. A way back.
stamp=$(date -u +%Y%m%dT%H%M%SZ)
if $snapshot; then
  if pct snapshot "$ct" "devagotchi-$stamp" --description "before hub-up.sh" >/dev/null 2>&1; then
    say "snapshot devagotchi-$stamp of CT $ct (undo all of this: pct rollback $ct devagotchi-$stamp --start)"
  else
    note "CT $ct's storage takes no snapshots; going on without one (back it up with vzdump to keep a way back)"
  fi
fi

# 2. Docker in the container: nesting, and keyctl when it is unprivileged.
features=$(sed -n 's/^features: //p' <<<"$config")
want="$features"
add_feature() { grep -q "\(^\|,\)$1=1" <<<"$want" || want="${want:+$want,}$1=1"; }
add_feature nesting
grep -q '^unprivileged: 1' <<<"$config" && add_feature keyctl
if [[ "$want" != "$features" ]]; then
  say "CT $ct: features $want (for Docker); restarting it"
  pct set "$ct" --features "$want"
  if pct status "$ct" | grep -q running; then pct reboot "$ct"; else pct start "$ct"; fi
elif ! pct status "$ct" | grep -q running; then
  pct start "$ct"
fi
for _ in $(seq 1 60); do
  in_ct sh -c 'getent hosts deb.debian.org >/dev/null' 2>/dev/null && break
  sleep 2
done
in_ct sh -c 'getent hosts deb.debian.org >/dev/null' || die "CT $ct has no working network or DNS"

# 3. The hub it ran before, off for good (its files stay where they are).
say "stopping a hub installed in CT $ct before, if any"
in_ct sh -c '
  if id devagotchi >/dev/null 2>&1; then
    uid=$(id -u devagotchi)
    runuser -u devagotchi -- env XDG_RUNTIME_DIR=/run/user/$uid \
      systemctl --user disable --now devagotchi-hub.service 2>/dev/null || true
  fi
  systemctl disable --now devagotchi-hub.service 2>/dev/null || true
'
if in_ct sh -c "ss -Hltn 'sport = :$port' | grep -q ." 2>/dev/null; then
  die "something in CT $ct still listens on port $port: $(in_ct ss -Hltnp "sport = :$port" | head -1)"
fi

# 4. The stack, from one commit of the deploy files.
sha=$(curl -fsSL "https://api.github.com/repos/$repo/commits/$ref" | sed -n 's/^  "sha": "\([0-9a-f]\{40\}\)".*/\1/p' | head -1)
[[ -n "$sha" ]] || die "could not resolve $repo@$ref"
edge=caddy
if in_ct sh -c "command -v caddy >/dev/null && grep -rlsqF -- '$domain' /etc/caddy" 2>/dev/null; then
  edge=external
  note "CT $ct's Caddy already serves $domain: the hub takes 127.0.0.1:$port behind it"
fi
say "installing the hub in CT $ct from $repo@${sha:0:12}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for file in install.sh compose.yaml update.sh backup.sh; do
  curl -fsSL "https://raw.githubusercontent.com/$repo/$sha/deploy/compose/$file" -o "$work/$file"
done
in_ct sh -c 'command -v curl >/dev/null || { apt-get update -q && DEBIAN_FRONTEND=noninteractive apt-get install -yq curl ca-certificates; }' >/dev/null
in_ct mkdir -p /root/devagotchi-hub-src
for file in install.sh compose.yaml update.sh backup.sh; do
  pct push "$ct" "$work/$file" "/root/devagotchi-hub-src/$file" --perms 0755
done
in_ct /root/devagotchi-hub-src/install.sh --domain "$domain" --rp-id "$rp_id" --edge "$edge" \
  --hub-bind 127.0.0.1 --hub-port "$port" --track "$track"

# 5. The public name answers from the new hub.
head_of() { grep -o '"head":"[0-9a-f]*"' | head -1; }
new_head=$(in_ct curl -fsS -m 10 "http://127.0.0.1:$port/healthz" 2>/dev/null | head_of || true)
[[ -n "$new_head" ]] || die "the new hub does not answer inside CT $ct (pct exec $ct -- sh -c 'cd /opt/devagotchi-hub && docker compose logs hub')"
say "checking https://$domain answers from the new hub"
ok=false
for _ in $(seq 1 30); do
  if [[ "$(curl -fsS -m 10 "https://$domain/healthz" 2>/dev/null | head_of)" == "$new_head" ]]; then ok=true; break; fi
  sleep 2
done
$ok || die "https://$domain/healthz does not answer from the new hub yet; it runs in CT $ct at 127.0.0.1:$port. Check the proxy in front of it"
note "https://$domain is the new hub"

claim=$(in_ct sh -c 'cd /opt/devagotchi-hub && docker compose logs --no-color hub 2>/dev/null' \
  | grep -o "https://$domain/claim#[0-9A-Z-]*" | tail -1 || true)
say "done: CT $ct runs the hub, deploys each new image of $track by itself, and backs up nightly"
if [[ -n "$claim" ]]; then
  printf '\nClaim it now: open this link on the phone or computer that will hold your passkey.\n  %s\n' "$claim"
fi
