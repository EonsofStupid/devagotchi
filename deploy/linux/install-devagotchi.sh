#!/usr/bin/env bash
# install-devagotchi.sh: Devagotchi on a Linux server, reached by AI through a hub.
#
# Installs the broker (owns this machine's jobs) and the link (one outbound connection to the
# hub) as systemd user services of a dedicated non-root account, started at boot by lingering.
# Nothing here listens on the network. Run it as root; the services never run as root.
#
#   sudo ./install-devagotchi.sh --version 0.2.0                  # from a GitHub release (sha256 checked)
#   sudo ./install-devagotchi.sh --bin-dir ./target/release       # from a local build
#   sudo ./install-devagotchi.sh --hub https://hub.example.com --device wi1 --enroll < code
#   sudo ./install-devagotchi.sh --uninstall
#
# The one-time enrollment code (from `devagotchi-hub enroll --device NAME`) is read from stdin,
# never from the command line, and reaches the link on its stdin. Re-running upgrades the
# binaries and keeps the config and the enrollment. Binaries live in root-owned
# <prefix>/versions/<version>; <prefix>/current points at one of them, so a rollback is one
# re-run with the previous --version.
# shellcheck disable=SC2016  # the `sh -c '...'` scripts take their arguments as $1 and $2
set -euo pipefail
umask 022

REPO=EonsofStupid/devagotchi
TARGET=x86_64-unknown-linux-gnu
PREFIX=/opt/devagotchi
ACCOUNT=devagotchi
ACCOUNT_HOME=/var/lib/devagotchi
VERSION='' BIN_DIR='' ARTIFACT_DIR='' WORKSPACE='' HUB='' DEVICE='' ENROLL=0 UNINSTALL=0
GRANTS=()

die() { echo "install-devagotchi: $*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }
usage() { sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; }

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

uid_of() { getent passwd "$ACCOUNT" | cut -d: -f3; }
home_of() { getent passwd "$ACCOUNT" | cut -d: -f6; }
UNITS=(devagotchi-link.service devagotchi-broker.service)

# Everything that writes into the account's home runs as that account, so root never follows
# a link the account could have planted there.
as_user() {
    local uid; uid=$(uid_of)
    runuser -u "$ACCOUNT" -- env -i HOME="$(home_of)" USER="$ACCOUNT" LOGNAME="$ACCOUNT" \
        PATH=/usr/local/bin:/usr/bin:/bin XDG_RUNTIME_DIR="/run/user/$uid" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" "$@"
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
        as_user rm -f "$home/.config/systemd/user/devagotchi-broker.service" "$home/.config/systemd/user/devagotchi-link.service"
        [ -S "/run/user/$(uid_of)/bus" ] && { user_systemctl daemon-reload || true; }
        loginctl disable-linger "$ACCOUNT" || true
        echo "    stopped and removed the services of $ACCOUNT; lingering is off"
        echo "    left in place: the account $ACCOUNT, $home (config, journal, the link's key), $PREFIX (binaries)"
        echo "    revoke this machine on the hub too: devagotchi-hub revoke --device <name>"
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
BINARIES=(bridge-broker bridge-mcp devagotchi-link)
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
    step "Devagotchi $VERSION for $TARGET"
    dest=$PREFIX/versions/$VERSION
    work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
    for app in devagotchi-broker devagotchi-mcp devagotchi-link; do
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

# ------------------------------------------------------------------ broker config
json_string() {
    case $1 in *[[:cntrl:]]*) die "path with a control character: $1" ;; esac
    local s=${1//\\/\\\\}; s=${s//\"/\\\"}; printf '"%s"' "$s"
}
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
# put_unit NAME TEXT: write the unit when its text differs; returns 0 when it changed.
put_unit() {
    local now; now=$(as_user cat "$UNIT_DIR/$1" 2>/dev/null || true)
    [ "$now" != "$2" ] || return 1
    printf '%s\n' "$2" | write_as_user "$UNIT_DIR/$1" 0644
}
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
