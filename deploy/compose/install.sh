#!/usr/bin/env bash
# Put the self-hosted hub on this Linux host and keep it on the newest image: Docker if it
# is missing, the stack from compose.yaml, the image pinned by digest, a timer that deploys
# each new image (and rolls back one that is not healthy), and a nightly backup.
#
#   sudo ./install.sh --domain hub.example.com                    # with this stack's Caddy
#   sudo ./install.sh --domain hub.example.com --edge external \
#        --hub-bind 10.0.0.20                                     # behind a proxy you run
#   sudo ./install.sh --domain app.example.com --tenancy multi \
#        --operators email:you@example.com --google --env-file /run/hub.env   # hosted
#
# Re-running it is safe: it rewrites .env from the flags, keeps the volumes, and deploys
# the image the track points at now. Nothing here is specific to one host or domain.
set -euo pipefail

usage() {
  cat <<'EOF'
install.sh --domain NAME [options]
  --domain NAME        the name people and AI clients reach the hub at (required)
  --public-url URL     when clients use another URL than https://NAME
  --rp-id NAME         the passkeys' relying party: NAME or a parent domain (default NAME)
  --edge caddy|external
                       caddy (default): this stack's Caddy takes ports 80 and 443 for NAME;
                       external: a proxy elsewhere terminates TLS and reaches the hub's port
  --hub-bind ADDR      where the hub's HTTP port is published (default 127.0.0.1)
  --hub-port PORT      that port (default 8787)
  --track IMAGE        the image tag to follow (default ghcr.io/eonsofstupid/devagotchi:main)
  --dir DIR            where the stack lives (default /opt/devagotchi-hub)
  --source DIR|URL     where compose.yaml and update.sh come from (default: beside this
                       script, else https://raw.githubusercontent.com/<repo>/<ref>/deploy/compose)
  --repo OWNER/NAME    (default EonsofStupid/devagotchi)   --ref REF  (default main)
  --no-timer           do not install the update and backup timers
A hosted hub, who runs it, and how people sign in (see crates/hub/README.md):
  --tenancy single|multi
                       single (default): one owner's hub, claimed from its log when no
                       operator is set; multi: a hosted service, an account for everyone who
                       connects, never claimed
  --operators LIST     who operates it, comma separated: email:ADDRESS or PROVIDER:SUBJECT
  --trusted-proxies LIST
                       proxies whose X-Forwarded-For names the client, comma separated
                       (default: private and loopback addresses)
  --google             Sign in with Google: DEVAGOTCHI_GOOGLE_CLIENT_ID and _SECRET
  --github             Sign in with GitHub: DEVAGOTCHI_GITHUB_CLIENT_ID and _SECRET
  --nomen ISSUER       Sign in with Shippin: DEVAGOTCHI_NOMEN_CLIENT_ID (and _SECRET if its app
                       has one), at the Nomen issuer ISSUER
  --email-from ADDRESS sign-in links by email from ADDRESS: DEVAGOTCHI_SMTP_URL
                       (smtps://user@host:465 or smtp://user@host:587) and DEVAGOTCHI_SMTP_PASSWORD
  --env-file FILE      read those variables from FILE (KEY=VALUE lines, as the secret store
                       writes them) rather than from this command's environment; a re-run
                       keeps the values .env already has for any it is not given
EOF
}

domain="" public_url="" rp_id="" edge=caddy hub_bind=127.0.0.1 hub_port=8787
track=ghcr.io/eonsofstupid/devagotchi:main dir=/opt/devagotchi-hub source=""
repo=EonsofStupid/devagotchi ref=main timer=true
tenancy="" operators="" proxies="" google=false github=false nomen="" email_from="" env_file=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --domain) domain="$2"; shift 2 ;;
    --public-url) public_url="$2"; shift 2 ;;
    --rp-id) rp_id="$2"; shift 2 ;;
    --edge) edge="$2"; shift 2 ;;
    --hub-bind) hub_bind="$2"; shift 2 ;;
    --hub-port) hub_port="$2"; shift 2 ;;
    --track) track="$2"; shift 2 ;;
    --dir) dir="$2"; shift 2 ;;
    --source) source="$2"; shift 2 ;;
    --repo) repo="$2"; shift 2 ;;
    --ref) ref="$2"; shift 2 ;;
    --no-timer) timer=false; shift ;;
    --tenancy) tenancy="$2"; shift 2 ;;
    --operators) operators="$2"; shift 2 ;;
    --trusted-proxies) proxies="$2"; shift 2 ;;
    --google) google=true; shift ;;
    --github) github=true; shift ;;
    --nomen) nomen="$2"; shift 2 ;;
    --email-from) email_from="$2"; shift 2 ;;
    --env-file) env_file="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; usage >&2; exit 2 ;;
  esac
done
say() { printf '==> %s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
[[ -n "$domain" ]] || { usage >&2; exit 2; }
[[ "$domain" =~ ^[A-Za-z0-9.-]+$ ]] || die "--domain $domain is not a host name"
[[ "$edge" == caddy || "$edge" == external ]] || die "--edge is caddy or external"
[[ "$hub_port" =~ ^[0-9]+$ ]] || die "--hub-port is a number"
[[ $EUID -eq 0 ]] || die "run it as root (it installs Docker and systemd units)"
domain="${domain,,}"; rp_id="${rp_id,,}"
public_url="${public_url:-https://$domain}"
rp_id="${rp_id:-$domain}"
[[ -z "$tenancy" || "$tenancy" == single || "$tenancy" == multi ]] || die "--tenancy is single or multi"
[[ "$operators" =~ ^[A-Za-z0-9@._+:,-]*$ ]] || die "--operators is email:ADDRESS or PROVIDER:SUBJECT, comma separated"
[[ "$proxies" =~ ^[0-9A-Fa-f.:/,]*$ ]] || die "--trusted-proxies is IP addresses or CIDR ranges, comma separated"
[[ -z "$nomen" || "$nomen" =~ ^https://[A-Za-z0-9.:/-]+$ ]] || die "--nomen is the Nomen issuer's https URL"
[[ -z "$email_from" || ( "$email_from" == *@* && "$email_from" != *[\'\"]* ) ]] \
  || die "--email-from is an address (Name <address> works too)"
[[ -z "$env_file" || -f "$env_file" ]] || die "--env-file $env_file is not a file"

# Docker Engine and the Compose plugin, from Docker's own repository when missing: the
# distributions' packages lag behind what compose.yaml needs (Compose 2.23+).
if ! docker compose version >/dev/null 2>&1; then
  say "installing Docker Engine and the Compose plugin"
  . /etc/os-release
  case "${ID:-}" in debian|ubuntu) ;; *) die "no Docker here and no installer for ${ID:-this OS}: install Docker Engine with Compose 2.23+ and re-run" ;; esac
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -q
  apt-get install -yq ca-certificates curl
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL "https://download.docker.com/linux/$ID/gpg" -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/$ID ${VERSION_CODENAME} stable" \
    > /etc/apt/sources.list.d/docker.list
  apt-get update -q
  apt-get install -yq docker-ce docker-ce-cli containerd.io docker-compose-plugin
  systemctl enable --now docker
fi
docker info >/dev/null 2>&1 || die "Docker is installed but not running: $(docker info 2>&1 | tail -1)"
command -v curl >/dev/null || { apt-get update -q && apt-get install -yq curl; }

# The stack's files: from beside this script (a checkout, or what an orchestrator copied
# in), or fetched from the repository at one ref.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -z "$source" ]]; then
  if [[ -f "$here/compose.yaml" && -f "$here/update.sh" ]]; then source="$here"
  else source="https://raw.githubusercontent.com/$repo/$ref/deploy/compose"; fi
fi
install -d -m 0755 "$dir"
for file in compose.yaml update.sh backup.sh; do
  if [[ "$source" == https://* ]]; then
    curl -fsSL "$source/$file" -o "$dir/$file.new"
  else
    cp "$source/$file" "$dir/$file.new"
  fi
  mv "$dir/$file.new" "$dir/$file"
done
chmod 0755 "$dir/update.sh" "$dir/backup.sh"

# Postgres sized for this box: a quarter of RAM for shared buffers, three quarters as the
# planner's cache estimate.
mem_mb=$(( $(awk '/^MemTotal:/ {print $2}' /proc/meminfo) / 1024 ))
shared=$(( mem_mb / 4 )); [[ $shared -lt 128 ]] && shared=128
cache=$(( mem_mb * 3 / 4 ))

# .env: rewritten from the flags each run; the image stays pinned to the digest that runs.
pinned=""
[[ -f "$dir/.env" ]] && pinned=$(sed -n 's/^DEVAGOTCHI_IMAGE=//p' "$dir/.env" | tail -1)

# The sign-in providers' ids and secrets: from --env-file, else this command's environment,
# else what .env already holds (a re-run). Never printed; written single-quoted to .env (0600).
secret_names=(DEVAGOTCHI_GOOGLE_CLIENT_ID DEVAGOTCHI_GOOGLE_CLIENT_SECRET DEVAGOTCHI_GITHUB_CLIENT_ID
  DEVAGOTCHI_GITHUB_CLIENT_SECRET DEVAGOTCHI_NOMEN_CLIENT_ID DEVAGOTCHI_NOMEN_CLIENT_SECRET
  DEVAGOTCHI_SMTP_URL DEVAGOTCHI_SMTP_PASSWORD)
declare -A secret=()
unquote() { sed "s/^'\(.*\)'\$/\1/; s/^\"\(.*\)\"\$/\1/"; }
from_file() { [[ -n "$env_file" ]] && sed -n "s/^\(export \)\{0,1\}$1=//p" "$env_file" | tail -1 | unquote; }
from_dotenv() { [[ -f "$dir/.env" ]] && sed -n "s/^$1=//p" "$dir/.env" | tail -1 | unquote; }
for name in "${secret_names[@]}"; do
  value=$(from_file "$name" || true)
  [[ -n "$value" ]] || value="${!name:-}"
  [[ -n "$value" ]] || value=$(from_dotenv "$name" || true)
  [[ "$value" != *"'"* && "$value" != *$'\n'* ]] || die "$name has a quote or a newline, which .env cannot hold"
  secret[$name]="$value"
done
need() { [[ -n "${secret[$1]}" ]] || die "$2 needs $1 (in the environment or --env-file)"; }
providers=()
if $google; then
  need DEVAGOTCHI_GOOGLE_CLIENT_ID --google; need DEVAGOTCHI_GOOGLE_CLIENT_SECRET --google
  providers+=('{"kind":"google","client_id_env":"DEVAGOTCHI_GOOGLE_CLIENT_ID","client_secret_env":"DEVAGOTCHI_GOOGLE_CLIENT_SECRET"}')
fi
if $github; then
  need DEVAGOTCHI_GITHUB_CLIENT_ID --github; need DEVAGOTCHI_GITHUB_CLIENT_SECRET --github
  providers+=('{"kind":"github","client_id_env":"DEVAGOTCHI_GITHUB_CLIENT_ID","client_secret_env":"DEVAGOTCHI_GITHUB_CLIENT_SECRET"}')
fi
if [[ -n "$nomen" ]]; then
  need DEVAGOTCHI_NOMEN_CLIENT_ID --nomen
  nomen_secret=""
  [[ -n "${secret[DEVAGOTCHI_NOMEN_CLIENT_SECRET]}" ]] && nomen_secret=',"client_secret_env":"DEVAGOTCHI_NOMEN_CLIENT_SECRET"'
  providers+=("{\"kind\":\"oidc\",\"name\":\"nomen\",\"label\":\"Shippin\",\"issuer\":\"${nomen%/}\",\"client_id_env\":\"DEVAGOTCHI_NOMEN_CLIENT_ID\"$nomen_secret}")
fi
email=null
if [[ -n "$email_from" ]]; then
  need DEVAGOTCHI_SMTP_URL --email-from
  password=""
  [[ -n "${secret[DEVAGOTCHI_SMTP_PASSWORD]}" ]] && password=',"password_env":"DEVAGOTCHI_SMTP_PASSWORD"'
  email="{\"smtp_url_env\":\"DEVAGOTCHI_SMTP_URL\"$password,\"from\":\"$email_from\"}"
fi
json_list() { local IFS=,; printf '[%s]' "$*"; }
proxy_list=()
IFS=, read -ra given <<<"$proxies"
for proxy in "${given[@]}"; do [[ -n "$proxy" ]] && proxy_list+=("\"$proxy\""); done
umask 077
cat > "$dir/.env.new" <<EOF
# Written by install.sh; re-run it to change these. update.sh moves DEVAGOTCHI_IMAGE.
DEVAGOTCHI_DOMAIN=$domain
DEVAGOTCHI_PUBLIC_URL=$public_url
DEVAGOTCHI_RP_ID=$rp_id
DEVAGOTCHI_HUB_BIND=$hub_bind
DEVAGOTCHI_HUB_PORT=$hub_port
DEVAGOTCHI_TRACK=$track
DEVAGOTCHI_EDGE=$edge
DEVAGOTCHI_PG_SHARED_BUFFERS=${shared}MB
DEVAGOTCHI_PG_EFFECTIVE_CACHE_SIZE=${cache}MB
DEVAGOTCHI_IMAGE=${pinned:-$track}
# The hub: one owner's or hosted, its operators, its sign-ins (install.sh's flags).
DEVAGOTCHI_HUB_TENANCY=${tenancy:-single}
DEVAGOTCHI_HUB_OPERATORS=$operators
DEVAGOTCHI_HUB_PROVIDERS='$(json_list "${providers[@]}")'
DEVAGOTCHI_HUB_EMAIL='$email'
EOF
if [[ ${#proxy_list[@]} -gt 0 ]]; then
  printf "DEVAGOTCHI_HUB_TRUSTED_PROXIES='%s'\n" "$(json_list "${proxy_list[@]}")" >> "$dir/.env.new"
fi
for name in "${secret_names[@]}"; do
  if [[ -n "${secret[$name]}" ]]; then printf "%s='%s'\n" "$name" "${secret[$name]}" >> "$dir/.env.new"; fi
done
mv "$dir/.env.new" "$dir/.env"
umask 022

# First start (or a re-run): the stack up, then update.sh pins the track's digest and
# deploys it, health-checked, exactly as the timer will every few minutes.
services=(postgres hub); [[ "$edge" == caddy ]] && services+=(caddy)
# Behind an external edge, a Caddy an earlier run started here would only hold ports 80/443.
[[ "$edge" == external ]] && (cd "$dir" && docker compose rm -sf caddy >/dev/null 2>&1 || true)
say "starting ${services[*]} in $dir"
(cd "$dir" && docker compose up -d --pull missing --wait --wait-timeout 180 "${services[@]}")
DEVAGOTCHI_DIR="$dir" "$dir/update.sh" --now || die "the hub did not come up healthy; see: cd $dir && docker compose logs hub"

if $timer; then
  say "installing the update timer (every 5 minutes) and the nightly backup"
  cat > /etc/systemd/system/devagotchi-hub-update.service <<EOF
[Unit]
Description=Deploy the newest Devagotchi hub image when it is healthy
After=docker.service
Requires=docker.service
[Service]
Type=oneshot
Environment=DEVAGOTCHI_DIR=$dir
ExecStart=$dir/update.sh
EOF
  cat > /etc/systemd/system/devagotchi-hub-update.timer <<'EOF'
[Unit]
Description=Check for a new Devagotchi hub image
[Timer]
OnBootSec=2min
OnUnitActiveSec=5min
RandomizedDelaySec=30s
[Install]
WantedBy=timers.target
EOF
  cat > /etc/systemd/system/devagotchi-hub-backup.service <<EOF
[Unit]
Description=Back up the Devagotchi hub's database and state
After=docker.service
Requires=docker.service
[Service]
Type=oneshot
Environment=DEVAGOTCHI_DIR=$dir
ExecStart=$dir/backup.sh
EOF
  cat > /etc/systemd/system/devagotchi-hub-backup.timer <<'EOF'
[Unit]
Description=Nightly backup of the Devagotchi hub
[Timer]
OnCalendar=*-*-* 03:17:00
RandomizedDelaySec=20min
Persistent=true
[Install]
WantedBy=timers.target
EOF
  systemctl daemon-reload
  systemctl enable --now devagotchi-hub-update.timer devagotchi-hub-backup.timer
fi

say "the hub is up at $public_url (here: http://$hub_bind:$hub_port)"
if [[ "$tenancy" == multi ]]; then
  printf '\nA hosted hub: nobody claims it. Each person gets an account the first time their AI connects to %s/mcp.\n' "$public_url"
fi
claim=$(cd "$dir" && docker compose logs --no-color hub 2>/dev/null | grep -o "https\?://[^ ]*/claim#[0-9A-Z-]*" | tail -1 || true)
if [[ -n "$claim" ]]; then
  printf '\nClaim it: open this link on the phone or computer that will hold your passkey.\n  %s\n(It works until you make the passkey; `cd %s && docker compose restart hub` prints a new one.)\n' "$claim" "$dir"
fi
