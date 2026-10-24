#!/usr/bin/env bash
# install-devagotchi.sh: Devagotchi on a Linux server, reached by AI through a hub.
#
# Role machine (default): the broker (owns this machine's jobs) and the link (one outbound
# connection to the hub) as systemd user services of a dedicated non-root account, started at
# boot by lingering. Nothing listens on the network. Role hub: the hub itself (the bastion:
# /mcp for agents, /link for machines, the project boards), listening on loopback behind Caddy
# for TLS. Run it as root; the services never run as root.
#
#   sudo ./install-devagotchi.sh --version 0.2.0                  # from a GitHub release (sha256 checked)
#   sudo ./install-devagotchi.sh --bin-dir ./target/release       # from a local build
#   sudo ./install-devagotchi.sh --hub https://hub.example.com --device wi1 --enroll < code
#   sudo ./install-devagotchi.sh --role hub --version 0.2.0 --public-url https://devagotchi.com \
#        --board devagotchi --person jesse --agent claude-on-gus --caddy
#   sudo ./install-devagotchi.sh --role hub --agent codex-on-dabo   # one more token (printed once)
#   sudo ./install-devagotchi.sh --uninstall
#   ./install-devagotchi.sh --print-target                       # the release build this machine takes
#
# The one-time enrollment code (from `devagotchi-hub enroll --device NAME`) is read from stdin,
# never from the command line, and reaches the link on its stdin. Re-running upgrades the
# binaries and keeps the config and the enrollment. Binaries live in root-owned
# <prefix>/versions/<version>; <prefix>/current points at one of them, so a rollback is one
# re-run with the previous --version. Releases exist for x86_64 and aarch64 (64-bit ARM).
# shellcheck disable=SC2016  # the `sh -c '...'` scripts take their arguments as $1 and $2
set -euo pipefail
umask 022

REPO=EonsofStupid/devagotchi
# The release build for this machine: the kernel's architecture, and a 64-bit userland (a
# 32-bit ARM system on a 64-bit kernel reports aarch64 but cannot run aarch64 binaries).
case $(uname -m) in
    x86_64|amd64) TARGET=x86_64-unknown-linux-gnu ;;
    aarch64|arm64) TARGET=aarch64-unknown-linux-gnu ;;
    *) TARGET='' ;;
esac
[ "$(getconf LONG_BIT 2>/dev/null || echo 64)" = 64 ] || TARGET=''
PREFIX=/opt/devagotchi
ACCOUNT=devagotchi
ACCOUNT_HOME=/var/lib/devagotchi
VERSION='' BIN_DIR='' ARTIFACT_DIR='' WORKSPACE='' HUB='' DEVICE='' ENROLL=0 UNINSTALL=0
ROLE=machine PUBLIC_URL='' LISTEN=127.0.0.1:8787 CADDY=0
GRANTS=() BOARDS=() PEOPLE=() AGENTS=()

die() { echo "install-devagotchi: $*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }
usage() { sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'; }
need_target() {
    [ -n "$TARGET" ] || die "no Devagotchi release for this machine ($(uname -m), $(getconf LONG_BIT 2>/dev/null || echo '?')-bit userland); releases cover 64-bit x86_64 and aarch64. Build it here and use --bin-dir"
}

while [ $# -gt 0 ]; do
    case $1 in
        --version) VERSION=${2:?--version needs a value}; shift 2 ;;
        --bin-dir) BIN_DIR=${2:?--bin-dir needs a value}; shift 2 ;;
        --artifact-dir) ARTIFACT_DIR=${2:?--artifact-dir needs a value}; shift 2 ;;
        --prefix) PREFIX=${2:?--prefix needs a value}; shift 2 ;;
        --user) ACCOUNT=${2:?--user needs a value}; shift 2 ;;
        --home) ACCOUNT_HOME=${2:?--home needs a value}; shift 2 ;;
        --workspace) WORKSPACE=${2:?--workspace needs a value}; shift 2 ;;
        --grant) GRANTS+=("${2:?--grant needs a value}"); shift 2 ;;
        --hub) HUB=${2:?--hub needs a value}; shift 2 ;;
        --device) DEVICE=${2:?--device needs a value}; shift 2 ;;
        --enroll) ENROLL=1; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        --repo) REPO=${2:?--repo needs a value}; shift 2 ;;
        --role) ROLE=${2:?--role needs machine or hub}; shift 2 ;;
        --public-url) PUBLIC_URL=${2:?--public-url needs a value}; shift 2 ;;
        --listen) LISTEN=${2:?--listen needs host:port}; shift 2 ;;
        --board) BOARDS+=("${2:?--board needs NAME or NAME=DIR}"); shift 2 ;;
        --person) PEOPLE+=("${2:?--person needs a name}"); shift 2 ;;
        --agent) AGENTS+=("${2:?--agent needs a name}"); shift 2 ;;
        --caddy) CADDY=1; shift ;;
        --print-target) need_target; echo "$TARGET"; exit 0 ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
done

# ------------------------------------------------------------------ preconditions
[ "$(id -u)" = 0 ] || die "run as root (sudo); the services themselves run as '$ACCOUNT'"
[ -d /run/systemd/system ] || die "systemd is not running as init here; this installer needs it"
for tool in runuser loginctl systemctl getent install; do
    command -v "$tool" >/dev/null || die "missing $tool"
done
case $ACCOUNT in root|'') die "--user must be a dedicated non-root account" ;; esac
[[ $ACCOUNT =~ ^[a-z_][a-z0-9_-]{0,31}$ ]] || die "--user $ACCOUNT is not a valid account name"
case $PREFIX in /*) ;; *) die "--prefix must be absolute" ;; esac
[[ $PREFIX =~ ^/[A-Za-z0-9._/-]+$ ]] || die "--prefix may hold only letters, digits and ._-/ (it goes into unit files)"
[ -z "$DEVICE" ] || [[ $DEVICE =~ ^[a-z0-9][a-z0-9-]{0,62}$ ]] || die "--device must be [a-z0-9][a-z0-9-]{0,62}, the broker name in the hub config"
[ $ENROLL = 0 ] || { [ -n "$HUB" ] && [ -n "$DEVICE" ]; } || die "--enroll needs --hub and --device"
[ -z "$VERSION" ] || [[ $VERSION =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || die "--version $VERSION is not a version"
case $ROLE in machine|hub) ;; *) die "--role must be machine or hub" ;; esac
if [ "$ROLE" = hub ]; then
    [ -z "$HUB$DEVICE" ] && [ $ENROLL = 0 ] || die "--hub, --device and --enroll belong to the machine role"
    [ -z "$PUBLIC_URL" ] || [[ $PUBLIC_URL =~ ^https?://[A-Za-z0-9.-]+(:[0-9]+)?$ ]] \
        || die "--public-url must be https://host or http://host:port, with no path"
    [[ $LISTEN =~ ^[A-Za-z0-9.:\[\]-]+:[0-9]+$ ]] || die "--listen must be host:port"
    for spec in "${BOARDS[@]}"; do
        [[ ${spec%%=*} =~ ^[a-z0-9][a-z0-9-]{0,62}$ ]] || die "--board ${spec%%=*}: a project name is [a-z0-9][a-z0-9-]{0,62}"
    done
    for name in "${PEOPLE[@]}" "${AGENTS[@]}"; do
        [[ $name =~ ^[^[:space:][:cntrl:]\"\\]{1,256}$ ]] || die "--person/--agent $name: no spaces, quotes or control characters"
    done
else
    [ -z "$PUBLIC_URL" ] && [ ${#BOARDS[@]} = 0 ] && [ ${#PEOPLE[@]} = 0 ] && [ ${#AGENTS[@]} = 0 ] && [ $CADDY = 0 ] \
        || die "--public-url, --board, --person, --agent and --caddy belong to --role hub"
fi

uid_of() { getent passwd "$ACCOUNT" | cut -d: -f3; }
home_of() { getent passwd "$ACCOUNT" | cut -d: -f6; }
UNITS=(devagotchi-link.service devagotchi-broker.service devagotchi-hub.service)

# Everything that writes into the account's home runs as that account, so root never follows
# a link the account could have planted there. From /, so a working directory the account
# cannot read never trips a command.
as_user() {
    local uid; uid=$(uid_of)
    runuser -u "$ACCOUNT" -- env -i HOME="$(home_of)" USER="$ACCOUNT" LOGNAME="$ACCOUNT" \
        PATH=/usr/local/bin:/usr/bin:/bin XDG_RUNTIME_DIR="/run/user/$uid" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" sh -c 'cd / && exec "$@"' sh "$@"
}
user_systemctl() { as_user systemctl --user "$@"; }
# write_as_user PATH MODE < content: atomic replace, as the account.
write_as_user() {
    as_user sh -c 'umask 077; mkdir -p "$(dirname "$1")"; cat > "$1.tmp" && chmod "$2" "$1.tmp" && mv -f "$1.tmp" "$1"' sh "$1" "$2"
}
wait_for_user_manager() {
    local uid; uid=$(uid_of)
    for _ in $(seq 1 60); do
        if [ -S "/run/user/$uid/bus" ] && user_systemctl show-environment >/dev/null 2>&1; then return 0; fi
        sleep 0.5
    done
    die "the user manager for $ACCOUNT did not start (loginctl enable-linger $ACCOUNT)"
}
# find_tool NAME: the first executable NAME in the system paths and the account's cargo bin.
find_tool() {
    local dir
    for dir in /usr/local/bin /usr/bin /bin "$HOME_DIR/.cargo/bin"; do
        if [ -f "$dir/$1" ] && [ -x "$dir/$1" ]; then echo "$dir/$1"; return 0; fi
    done
    return 1
}

if [ $UNINSTALL = 1 ]; then
    step "Uninstall"
    if getent passwd "$ACCOUNT" >/dev/null; then
        home=$(home_of)
        if [ -S "/run/user/$(uid_of)/bus" ]; then
            user_systemctl disable --now "${UNITS[@]}" 2>/dev/null || true
        fi
        as_user rm -f "$home/.config/systemd/user/devagotchi-broker.service" "$home/.config/systemd/user/devagotchi-link.service" \
            "$home/.config/systemd/user/devagotchi-hub.service"
        [ -S "/run/user/$(uid_of)/bus" ] && { user_systemctl daemon-reload || true; }
        loginctl disable-linger "$ACCOUNT" || true
        echo "    stopped and removed the services of $ACCOUNT; lingering is off"
        echo "    left in place: the account $ACCOUNT, $home (configs, journals, keys, boards), $PREFIX (binaries)"
        echo "    a machine: revoke it on its hub too (devagotchi-hub revoke --device <name>)"
        if [ -f /etc/caddy/devagotchi-hub.caddy ]; then
            echo "    a hub: /etc/caddy/devagotchi-hub.caddy and its import line in /etc/caddy/Caddyfile are left for you to remove"
        fi
    else
        echo "    no account $ACCOUNT; nothing to stop"
    fi
    exit 0
fi

[ -n "$VERSION" ] || [ -n "$BIN_DIR" ] || [ -L "$PREFIX/current" ] \
    || die "give --version or --bin-dir for the first install"

# The code is read before anything else runs, so no other command can consume stdin first.
CODE=''
if [ $ENROLL = 1 ]; then
    if [ -t 0 ]; then
        read -rsp "One-time enrollment code from the hub operator: " CODE; echo
    else
        IFS= read -r CODE || true
    fi
    [[ $CODE =~ ^dvg-[0-9a-f]{32}$ ]] || die "that is not an enrollment code (dvg- and 32 hex digits)"
fi

# ------------------------------------------------------------------ account
step "Account $ACCOUNT"
if ! getent passwd "$ACCOUNT" >/dev/null; then
    case $ACCOUNT_HOME in /*) ;; *) die "--home must be absolute" ;; esac
    useradd --system --create-home --home-dir "$ACCOUNT_HOME" --shell /bin/bash \
        --comment "Devagotchi broker and link" "$ACCOUNT"
    echo "    created system account $ACCOUNT (home $ACCOUNT_HOME, no password)"
else
    echo "    using existing account $ACCOUNT (home $(home_of))"
fi
[ "$(uid_of)" != 0 ] || die "$ACCOUNT has uid 0"
HOME_DIR=$(home_of)
as_user chmod go-w "$HOME_DIR" || die "$ACCOUNT does not own its home $HOME_DIR"
if id -nG "$ACCOUNT" | tr ' ' '\n' | grep -qxE 'sudo|wheel|admin'; then
    echo "    note: $ACCOUNT is in an admin group; anything an AI runs through this broker can use it"
fi
STATE=$HOME_DIR/.local/state/devagotchi
LINK_STATE=$HOME_DIR/.local/state/devagotchi-link
CONFIG=$HOME_DIR/.config/devagotchi/broker.json
UNIT_DIR=$HOME_DIR/.config/systemd/user
SOCKET=$STATE/broker.sock
as_user sh -c 'umask 077; mkdir -p "$1"' sh "$HOME_DIR/.local/state"

# ------------------------------------------------------------------ binaries
# Every version directory holds all four, whichever role this run installs, so a host
# that runs both roles from one prefix never has `current` point at a half set.
BINARIES=(bridge-broker bridge-mcp devagotchi-link devagotchi-hub)
install -d -o root -g root -m 0755 "$PREFIX" "$PREFIX/versions"
if [ -n "$BIN_DIR" ]; then
    step "Binaries from build directory $BIN_DIR"
    if [ -z "$VERSION" ]; then
        VERSION=local-$(date -u +%Y%m%d%H%M%S)
        n=1; base=$VERSION
        while [ -e "$PREFIX/versions/$VERSION" ]; do VERSION=$base-$n; n=$((n + 1)); done
    fi
    dest=$PREFIX/versions/$VERSION
    install -d -o root -g root -m 0755 "$dest"
    for bin in "${BINARIES[@]}"; do
        [ -f "$BIN_DIR/$bin" ] || die "missing $BIN_DIR/$bin (cargo build --locked --bins)"
        install -o root -g root -m 0755 "$BIN_DIR/$bin" "$dest/$bin"
    done
elif [ -n "$VERSION" ]; then
    need_target
    step "Devagotchi $VERSION for $TARGET"
    dest=$PREFIX/versions/$VERSION
    work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
    for app in devagotchi-broker devagotchi-mcp devagotchi-link devagotchi-hub; do
        archive=$app-$TARGET.tar.xz
        for name in "$archive" "$archive.sha256"; do
            if [ -n "$ARTIFACT_DIR" ]; then
                cp "$ARTIFACT_DIR/$name" "$work/$name"
            else
                command -v curl >/dev/null || die "missing curl (or pass --artifact-dir)"
                curl --proto '=https' --tlsv1.2 -fsSL -o "$work/$name" \
                    "https://github.com/$REPO/releases/download/v$VERSION/$name"
            fi
        done
        expected=$(cut -d' ' -f1 < "$work/$archive.sha256" | tr 'A-F' 'a-f')
        actual=$(sha256sum "$work/$archive" | cut -d' ' -f1)
        [ "$expected" = "$actual" ] || die "checksum mismatch for $archive: expected $expected, got $actual"
        echo "    sha256 ok: $archive"
        if [ -z "$ARTIFACT_DIR" ] && command -v gh >/dev/null; then
            if gh attestation verify "$work/$archive" --repo "$REPO" >/dev/null 2>&1; then
                echo "    provenance verified: $archive"
            else
                echo "    (provenance not verified for $archive)"
            fi
        fi
        mkdir -p "$work/$app" && tar -xJf "$work/$archive" -C "$work/$app"
    done
    install -d -o root -g root -m 0755 "$dest"
    for bin in "${BINARIES[@]}"; do
        found=$(find "$work" -type f -name "$bin" | head -n 1)
        [ -n "$found" ] || die "$bin is not in the release archives"
        install -o root -g root -m 0755 "$found" "$dest/$bin"
    done
else
    dest=$(readlink -f "$PREFIX/current")
    VERSION=$(basename "$dest")
    step "Keeping the installed version $VERSION"
fi
for bin in "${BINARIES[@]}"; do [ -x "$dest/$bin" ] || die "$dest/$bin is missing"; done
# A service restarts only when something it runs changed: on the subtree and process-group
# rungs a broker restart ends its jobs (plan section 5), so a re-run that only enrolls leaves
# the broker alone.
PREVIOUS=$(readlink "$PREFIX/current" 2>/dev/null || true)
BROKER_CHANGED=0; LINK_CHANGED=0
if [ "$PREVIOUS" != "versions/$VERSION" ]; then BROKER_CHANGED=1; LINK_CHANGED=1; fi
# Root-owned and not writable by the account: what runs as the account cannot replace itself.
ln -sfn "versions/$VERSION" "$PREFIX/.current.new"
mv -Tf "$PREFIX/.current.new" "$PREFIX/current"
echo "    $PREFIX/current -> versions/$VERSION"

# ------------------------------------------------------------------ user manager
step "User services for $ACCOUNT (lingering: they start at boot with nobody logged in)"
loginctl enable-linger "$ACCOUNT"
wait_for_user_manager

json_string() {
    case $1 in *[[:cntrl:]]*) die "path with a control character: $1" ;; esac
    local s=${1//\\/\\\\}; s=${s//\"/\\\"}; printf '"%s"' "$s"
}
# put_unit NAME TEXT: write the unit when its text differs; returns 0 when it changed.
put_unit() {
    local now; now=$(as_user cat "$UNIT_DIR/$1" 2>/dev/null || true)
    [ "$now" != "$2" ] || return 1
    printf '%s\n' "$2" | write_as_user "$UNIT_DIR/$1" 0644
}

# ================================================================== role hub
if [ "$ROLE" = hub ]; then
    HUB_CONFIG=$HOME_DIR/.config/devagotchi/hub.json
    TOKENS=$HOME_DIR/.config/devagotchi/hub-tokens.json
    HUB_CHANGED=$BROKER_CHANGED

    # ---- boards: one per project, initialised as the account so the hub can write them
    board_names=() board_entries=''
    if [ ${#BOARDS[@]} -gt 0 ]; then
        step "Boards"
        for spec in "${BOARDS[@]}"; do
            name=${spec%%=*}; dir=${spec#*=}
            [ "$dir" != "$spec" ] || dir=$HOME_DIR/projects/$name
            case $dir in /*) ;; *) die "--board $name: the directory must be absolute" ;; esac
            case $dir in "$HOME_DIR"/*) as_user sh -c 'umask 077; mkdir -p "$1"' sh "$dir" ;; esac
            as_user test -d "$dir" -a -w "$dir" -a -x "$dir" \
                || die "$ACCOUNT cannot write the project directory $dir (chown it, or keep boards under $HOME_DIR/projects)"
            as_user "$dest/devagotchi-hub" board init --dir "$dir" --project "$name" --agents-md --quiet >/dev/null \
                || die "board init failed for $name in $dir"
            board_names+=("$name")
            board_entries+="${board_entries:+, }{ \"project\": $(json_string "$name"), \"dir\": $(json_string "$dir") }"
            echo "    $name: $dir/.devagotchi/board.sqlite3 (AGENTS.md block in place)"
        done
    fi
    board_list=''
    for name in "${board_names[@]}"; do board_list+="${board_list:+, }$(json_string "$name")"; done

    # ---- config: written once; afterwards it is the operator's file
    step "Hub config"
    grant_line() {  # grant_line NAME HUMAN
        printf '{ "subject": %s, "boards": [%s], "human": %s }' "$(json_string "$1")" "$board_list" "$2"
    }
    if as_user test -e "$HUB_CONFIG"; then
        echo "    kept existing $HUB_CONFIG"
        if [ ${#board_names[@]} -gt 0 ]; then
            echo "    add to \"boards\" there (then restart the hub): $board_entries"
        fi
        for name in "${PEOPLE[@]}"; do echo "    add to \"grants\": $(grant_line "$name" true)"; done
        for name in "${AGENTS[@]}"; do echo "    add to \"grants\": $(grant_line "$name" false)"; done
    else
        [ -n "$PUBLIC_URL" ] || die "--public-url https://… is required the first time: the hub's address as agents and machines see it"
        as_user test -s "$TOKENS" || [ ${#PEOPLE[@]} -gt 0 ] || [ ${#AGENTS[@]} -gt 0 ] \
            || die "the first install needs at least one --person or --agent: the hub refuses to start with nobody to let in"
        grants=''
        for name in "${PEOPLE[@]}"; do grants+="${grants:+, }$(grant_line "$name" true)"; done
        for name in "${AGENTS[@]}"; do grants+="${grants:+, }$(grant_line "$name" false)"; done
        {
            printf '{\n  "listen": %s,\n  "public_url": %s,\n' "$(json_string "$LISTEN")" "$(json_string "$PUBLIC_URL")"
            printf '  "auth": { "mode": "dev", "tokens_file": %s },\n' "$(json_string "$TOKENS")"
            printf '  "brokers": [],\n  "boards": [%s],\n  "grants": [%s]\n}\n' "$board_entries" "$grants"
        } | write_as_user "$HUB_CONFIG" 0600
        HUB_CHANGED=1
        echo "    wrote $HUB_CONFIG: listen $LISTEN, public_url $PUBLIC_URL, ${#board_names[@]} boards, $(( ${#PEOPLE[@]} + ${#AGENTS[@]} )) grants"
        echo "    machines join by adding {\"name\": \"<device>\", \"link\": true} to \"brokers\" and a grant, then devagotchi-hub enroll"
    fi

    # ---- tokens: minted here, shown once, never logged
    if [ ${#PEOPLE[@]} -gt 0 ] || [ ${#AGENTS[@]} -gt 0 ]; then
        step "Tokens (shown once; put each in that agent's MCP config as the bearer token)"
        command -v python3 >/dev/null || die "minting tokens needs python3 (or add them to $TOKENS by hand: {\"<token>\": \"<subject>\"})"
        as_user test -e "$TOKENS" || printf '{}\n' | write_as_user "$TOKENS" 0600
        for name in "${PEOPLE[@]}" "${AGENTS[@]}"; do
            token=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')
            as_user python3 - "$TOKENS" "$token" "$name" <<'PY' || die "could not update $TOKENS"
import json, os, sys
path, token, subject = sys.argv[1:4]
with open(path) as f:
    tokens = json.load(f)
if not isinstance(tokens, dict):
    sys.exit("tokens file is not a JSON object")
tokens[token] = subject
tmp = path + ".tmp"
with open(os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as f:
    json.dump(tokens, f, indent=2)
    f.write("\n")
os.replace(tmp, path)
PY
            echo "    $name: $token"
        done
        echo "    the hub reads $TOKENS at each request; no restart needed"
    fi

    # ---- unit
    step "Unit in $UNIT_DIR"
    HUB_UNIT=$(cat <<EOF
# Written by install-devagotchi.sh; re-running the installer rewrites it.
[Unit]
Description=Devagotchi hub (the bastion: /mcp for agents, /link for machines, the boards)
Documentation=https://github.com/$REPO/tree/main/deploy/linux

[Service]
Type=simple
ExecStart=$PREFIX/current/devagotchi-hub --state-dir %h/.local/state/devagotchi-hub --config %h/.config/devagotchi/hub.json
WorkingDirectory=%h
Restart=always
RestartSec=2
UMask=0077

[Install]
WantedBy=default.target
EOF
)
    if put_unit devagotchi-hub.service "$HUB_UNIT"; then HUB_CHANGED=1; echo "    wrote devagotchi-hub.service"; fi
    user_systemctl daemon-reload

    step "Starting the hub"
    user_systemctl enable devagotchi-hub.service >/dev/null
    if [ $HUB_CHANGED = 1 ]; then
        user_systemctl restart devagotchi-hub.service
    else
        user_systemctl start devagotchi-hub.service
        echo "    nothing it runs changed: left running"
    fi
    hub_healthy() {
        if command -v curl >/dev/null; then curl -fsS -m 2 "http://$LISTEN/healthz" >/dev/null 2>&1
        elif command -v python3 >/dev/null; then python3 -c "import urllib.request,sys; urllib.request.urlopen('http://$LISTEN/healthz', timeout=2)" >/dev/null 2>&1
        else user_systemctl is-active --quiet devagotchi-hub.service; fi
    }
    for _ in $(seq 1 40); do hub_healthy && break; sleep 0.5; done
    if ! hub_healthy; then
        journalctl --no-pager -n 20 _UID="$(uid_of)" _SYSTEMD_USER_UNIT=devagotchi-hub.service || true
        die "the hub did not come up (systemctl --user status devagotchi-hub as $ACCOUNT)"
    fi
    echo "    healthy: http://$LISTEN/healthz"

    # ---- Caddy: TLS at the edge, the hub stays on loopback
    if [ $CADDY = 1 ]; then
        public=$(as_user python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["public_url"])' "$HUB_CONFIG" 2>/dev/null || echo "$PUBLIC_URL")
        [ -n "$public" ] || die "--caddy needs the hub's public_url (from the config, or --public-url)"
        step "Caddy at $public"
        if ! command -v caddy >/dev/null; then
            command -v apt-get >/dev/null || die "caddy is not installed; install it (https://caddyserver.com/docs/install), then re-run with --caddy"
            DEBIAN_FRONTEND=noninteractive apt-get update -q >/dev/null
            DEBIAN_FRONTEND=noninteractive apt-get install -y -q caddy >/dev/null
            echo "    installed caddy from the distribution"
        fi
        case $public in
            https://*) site=${public#https://} ;;      # Caddy fetches and renews the certificate itself
            http://*) site=$public ;;                  # plain http, only for a loopback or test address
        esac
        site_file=/etc/caddy/devagotchi-hub.caddy
        printf '# Written by install-devagotchi.sh: the Devagotchi hub behind this edge.\n%s {\n    reverse_proxy %s\n}\n' "$site" "$LISTEN" > "$site_file.tmp"
        chmod 0644 "$site_file.tmp" && mv -f "$site_file.tmp" "$site_file"
        [ -f /etc/caddy/Caddyfile ] || install -m 0644 /dev/null /etc/caddy/Caddyfile
        grep -qx "import $site_file" /etc/caddy/Caddyfile || printf '\nimport %s\n' "$site_file" >> /etc/caddy/Caddyfile
        caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile >/dev/null || die "caddy validate failed on /etc/caddy/Caddyfile"
        systemctl enable --now caddy >/dev/null 2>&1 || true
        systemctl reload caddy || systemctl restart caddy
        echo "    $site_file: $site -> $LISTEN; Caddyfile imports it"
        case $public in https://*) echo "    DNS: point ${site%%:*} at this host (A/AAAA) and open 80/443; the certificate follows by itself" ;; esac
    fi

    step "Done"
    echo "    Version:   $VERSION ($PREFIX/current)"
    echo "    Account:   $ACCOUNT, service: systemctl --user status devagotchi-hub (as $ACCOUNT)"
    echo "    Hub:       http://$LISTEN (loopback) behind ${PUBLIC_URL:-the public_url in $HUB_CONFIG}"
    echo "    Config:    $HUB_CONFIG (boards, brokers, grants; restart the hub after editing)"
    echo "    Agents:    MCP endpoint <public_url>/mcp with a bearer token; start with whoami, then board_prime"
    exit 0
fi

# ================================================================== role machine
# ------------------------------------------------------------------ broker config
step "Broker config"
if as_user test -e "$CONFIG"; then
    echo "    kept existing $CONFIG"
else
    [ -n "$WORKSPACE" ] || WORKSPACE=$HOME_DIR/workspace
    case $WORKSPACE in /*) ;; *) die "--workspace must be absolute" ;; esac
    case $WORKSPACE in "$HOME_DIR"/*) as_user sh -c 'umask 077; mkdir -p "$1"' sh "$WORKSPACE" ;; esac
    as_user test -d "$WORKSPACE" -a -r "$WORKSPACE" -a -x "$WORKSPACE" \
        || die "$ACCOUNT cannot use the workspace $WORKSPACE"
    candidates=()
    for name in git rg python3 node cargo rustup go uv; do
        if found=$(find_tool "$name"); then candidates+=("$found"); fi
    done
    candidates+=("${GRANTS[@]}")
    grants=()
    for path in "${candidates[@]}"; do
        real=$(readlink -f -- "$path" || true)
        if [ -z "$real" ] || [ ! -f "$real" ] || [ ! -x "$real" ]; then
            die "grant $path is not an executable file"
        fi
        case " ${grants[*]} " in *" $real "*) ;; *) grants+=("$real") ;; esac
    done
    list=''
    for real in "${grants[@]}"; do list+="${list:+, }$(json_string "$real")"; done
    printf '{\n  "workspace": %s,\n  "allowed_executables": [%s],\n  "output_limit_bytes": 262144,\n  "max_operations": 64,\n  "max_concurrent": 4\n}\n' \
        "$(json_string "$WORKSPACE")" "$list" | write_as_user "$CONFIG" 0600
    BROKER_CHANGED=1
    echo "    wrote $CONFIG: workspace $WORKSPACE, ${#grants[@]} grants"
    for real in "${grants[@]}"; do echo "      $real"; done
fi

# ------------------------------------------------------------------ units
step "Units in $UNIT_DIR"
BROKER_UNIT=$(cat <<EOF
# Written by install-devagotchi.sh; re-running the installer rewrites it.
[Unit]
Description=Devagotchi broker (owns this machine's jobs)
Documentation=https://github.com/$REPO/tree/main/deploy/linux

[Service]
Type=simple
ExecStart=$PREFIX/current/bridge-broker --state-dir %h/.local/state/devagotchi --config %h/.config/devagotchi/broker.json
WorkingDirectory=%h
# always, not on-failure: on-failure skips SIGTERM and clean exits.
Restart=always
RestartSec=1
# The broker's own cgroup subtree is delegated so each job can live in a child cgroup.
Delegate=yes
# Jobs are the broker's to settle; systemd kills the control group only if the broker fails to.
KillMode=mixed
TimeoutStopSec=30
UMask=0077

[Install]
WantedBy=default.target
EOF
)
LINK_UNIT=$(cat <<EOF
# Written by install-devagotchi.sh; re-running the installer rewrites it.
[Unit]
Description=Devagotchi link (one outbound connection to the hub; nothing listens here)
Documentation=https://github.com/$REPO/tree/main/deploy/linux
After=devagotchi-broker.service
Wants=devagotchi-broker.service
ConditionPathExists=%h/.local/state/devagotchi-link/link.json

[Service]
Type=simple
ExecStart=$PREFIX/current/devagotchi-link run --state-dir %h/.local/state/devagotchi-link
# The link reconnects on its own with backoff; this only covers a crash.
Restart=always
RestartSec=5
# Exit 2: the hub refused this machine (revoked or unknown). Restarting would be refused again.
RestartPreventExitStatus=2
UMask=0077

[Install]
WantedBy=default.target
EOF
)
if put_unit devagotchi-broker.service "$BROKER_UNIT"; then BROKER_CHANGED=1; echo "    wrote devagotchi-broker.service"; fi
if put_unit devagotchi-link.service "$LINK_UNIT"; then LINK_CHANGED=1; echo "    wrote devagotchi-link.service"; fi
user_systemctl daemon-reload

step "Starting the broker"
user_systemctl enable devagotchi-broker.service >/dev/null
if [ $BROKER_CHANGED = 1 ]; then
    user_systemctl restart devagotchi-broker.service
else
    user_systemctl start devagotchi-broker.service
    echo "    nothing it runs changed: left running"
fi
for _ in $(seq 1 40); do
    as_user test -S "$SOCKET" && user_systemctl is-active --quiet devagotchi-broker.service && break
    sleep 0.5
done
if ! as_user test -S "$SOCKET"; then
    journalctl --no-pager -n 20 _UID="$(uid_of)" _SYSTEMD_USER_UNIT=devagotchi-broker.service || true
    die "the broker did not come up (systemctl --user status devagotchi-broker as $ACCOUNT)"
fi
echo "    ready: $SOCKET"

# ------------------------------------------------------------------ link
if [ $ENROLL = 1 ]; then
    step "Enrolling this machine as '$DEVICE' on $HUB"
    # The code goes to the link on its stdin: never an argument, a file or a log line.
    if ! printf '%s\n' "$CODE" | as_user "$PREFIX/current/devagotchi-link" enroll --state-dir "$LINK_STATE" \
        --hub "$HUB" --device "$DEVICE" --broker "$SOCKET"; then
        unset CODE
        die "enrollment failed; ask the hub operator for a fresh code"
    fi
    unset CODE
    LINK_CHANGED=1
elif [ -n "$HUB" ] || [ -n "$DEVICE" ]; then
    as_user test -f "$LINK_STATE/link.json" || die "this machine has not enrolled yet: add --enroll and pipe the code once"
fi
if as_user test -f "$LINK_STATE/link.json"; then
    step "Starting the link"
    user_systemctl enable devagotchi-link.service >/dev/null
    if [ $LINK_CHANGED = 1 ]; then
        user_systemctl restart devagotchi-link.service
    else
        user_systemctl start devagotchi-link.service
    fi
    sleep 1
    user_systemctl is-active --quiet devagotchi-link.service || die "the link is not running (journalctl _SYSTEMD_USER_UNIT=devagotchi-link.service)"
    echo "    running; it reconnects by itself and starts at boot"
else
    echo
    echo "    No link yet. To join a hub: sudo $0 --hub <url> --device <name> --enroll < code"
fi

step "Done"
echo "    Version:   $VERSION ($PREFIX/current)"
echo "    Account:   $ACCOUNT, services: systemctl --user status devagotchi-broker devagotchi-link (as $ACCOUNT)"
echo "    Broker:    $SOCKET"
echo "    Local MCP: $PREFIX/current/bridge-mcp --socket $SOCKET (as $ACCOUNT)"
