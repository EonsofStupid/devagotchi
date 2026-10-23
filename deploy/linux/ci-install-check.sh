#!/usr/bin/env bash
# CI driver for install-devagotchi.sh on a real systemd host (.github/workflows/ci.yml,
# host-install job). Needs passwordless sudo and the debug build in target/debug.
#
# Checks, in order: install from the build as a dedicated account with lingering; the
# services run as that account from root-owned binaries; a real operation through the
# broker; the broker comes back after kill -9; the link joins a hub with a code piped
# through the installer (never on a command line) and a real operation runs through the
# hub and the link; an upgrade keeps the config and the enrollment; the link comes back
# after kill -9; a revoke makes the link exit 2 and systemd leaves it down; uninstall stops
# everything. Exit 0 only when every step holds.
set -euo pipefail

BIN=$(readlink -f target/debug)
INSTALL=$(readlink -f deploy/linux/install-devagotchi.sh)
ACCOUNT=dvg-ci
HOME_DIR=/var/lib/dvg-ci
PREFIX=/opt/dvg-ci
SOCKET=$HOME_DIR/.local/state/devagotchi/broker.sock
WORKSPACE=$HOME_DIR/workspace
COMMON=(--user "$ACCOUNT" --home "$HOME_DIR" --prefix "$PREFIX")
WORK=$(mktemp -d)
PROBES=/tmp/dvg-ci-probes
HUB_PID='' PORT=''

ok() { echo "ok   $*"; }
fail() { echo "FAIL $*"; exit 1; }
cleanup() {
    [ -z "$HUB_PID" ] || kill "$HUB_PID" 2>/dev/null || true
    rm -rf "$WORK"
}
trap cleanup EXIT

# As the account, from a directory it can read (the runner's checkout is not one).
as_account() { (cd / && sudo -u "$ACCOUNT" env XDG_RUNTIME_DIR="/run/user/$(id -u "$ACCOUNT")" "$@"); }
uctl() { as_account systemctl --user "$@"; }
prop() { uctl show -p "$2" --value "$1"; }
# wait_for SECONDS DESCRIPTION CHECK...: poll every half second until CHECK succeeds.
wait_for() {
    local seconds=$1 what=$2; shift 2
    for _ in $(seq 1 $((seconds * 2))); do
        if "$@" >/dev/null 2>&1; then return 0; fi
        sleep 0.5
    done
    fail "$what (waited ${seconds}s)"
}
broker_up() { [ "$(prop devagotchi-broker.service ActiveState)" = active ] && as_account test -S "$SOCKET"; }
new_main_pid() {  # new_main_pid UNIT OLD: running again under another, real pid
    local now; now=$(prop "$1" MainPID)
    [ "$now" != 0 ] && [ "$now" != "$2" ] && [ "$(prop "$1" ActiveState)" = active ]
}
link_down() { [ "$(prop devagotchi-link.service ActiveState)" != active ]; }
hub_ready() { grep -q 'devagotchi-hub ready:' "$WORK/hub.log"; }
no_processes_left() { ! pgrep -u "$ACCOUNT" -f 'bridge-broker|devagotchi-link'; }
link_connected() {
    curl -s "http://127.0.0.1:$PORT/healthz" | python3 -c \
        'import json,sys; b=[x for x in json.load(sys.stdin)["brokers"] if x["name"]=="ci-server"]; sys.exit(0 if b and b[0].get("connected") else 1)'
}
config_hash() { sudo sha256sum "$HOME_DIR/.config/devagotchi/broker.json" | cut -d' ' -f1; }

sudo install -d -m 0755 "$PROBES"
sudo install -m 0644 tests/adapter_probe.py tests/hub_probe.py "$PROBES/"

echo "--- install from the build"
sudo "$INSTALL" --bin-dir "$BIN" "${COMMON[@]}" --grant /usr/bin/git
broker_up || fail "broker not up"
[ "$(uctl is-enabled devagotchi-broker.service)" = enabled ] || fail "broker not enabled"
[ "$(loginctl show-user "$ACCOUNT" -p Linger --value)" = yes ] || fail "lingering is off"
ok "broker active and enabled; lingering on (starts at boot with nobody logged in)"
pid=$(prop devagotchi-broker.service MainPID)
[ "$(ps -o user= -p "$pid" | tr -d ' ')" = "$ACCOUNT" ] || fail "broker pid $pid does not run as $ACCOUNT"
[ "$(stat -c '%U %a' "$PREFIX/current/bridge-broker")" = "root 755" ] || fail "binaries are not root-owned 0755"
as_account test ! -w "$PREFIX/current/bridge-broker" || fail "$ACCOUNT can write its own binary"
[ "$(sudo stat -c '%U %a' "$HOME_DIR/.config/devagotchi/broker.json")" = "$ACCOUNT 600" ] || fail "config is not private"
ok "runs as $ACCOUNT from root-owned binaries; config 0600"
link_down || fail "link runs before enrollment"
ok "no link before enrollment"

echo "--- a real operation through the broker"
as_account python3 "$PROBES/adapter_probe.py" "$SOCKET" --adapter "$PREFIX/current/bridge-mcp" \
    --git /usr/bin/git --cwd "$WORKSPACE"
ok "git --version through the installed broker"

echo "--- the broker comes back after kill -9"
sudo kill -9 "$pid"
wait_for 20 "broker restarted under a new pid" new_main_pid devagotchi-broker.service "$pid"
wait_for 20 "broker listening again" broker_up
ok "restarted by systemd (pid $pid -> $(prop devagotchi-broker.service MainPID))"

echo "--- the link joins a hub"
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
TOKEN=ci-host-$(python3 -c 'import secrets; print(secrets.token_hex(16))')
install -m 0600 /dev/null "$WORK/tokens.json"
printf '{"%s":"ci"}' "$TOKEN" > "$WORK/tokens.json"
cat > "$WORK/hub.json" <<EOF
{"listen": "127.0.0.1:$PORT", "public_url": "http://127.0.0.1:$PORT",
 "auth": {"mode": "dev", "tokens_file": "$WORK/tokens.json"},
 "brokers": [{"name": "ci-server", "link": true}],
 "grants": [{"subject": "ci", "brokers": ["ci-server"]}]}
EOF
"$BIN/devagotchi-hub" --state-dir "$WORK/hub-state" --config "$WORK/hub.json" 2> "$WORK/hub.log" &
HUB_PID=$!
wait_for 20 "hub ready" hub_ready
broker_pid=$(prop devagotchi-broker.service MainPID)
code=$("$BIN/devagotchi-hub" enroll --state-dir "$WORK/hub-state" --device ci-server 2>/dev/null)
[[ $code == dvg-* ]] || fail "no enrollment code"
printf '%s\n' "$code" | sudo "$INSTALL" "${COMMON[@]}" --hub "http://127.0.0.1:$PORT" --device ci-server --enroll
unset code
wait_for 30 "hub sees the link" link_connected
[ "$(uctl is-enabled devagotchi-link.service)" = enabled ] || fail "link not enabled"
[ "$(prop devagotchi-broker.service MainPID)" = "$broker_pid" ] || fail "enrolling restarted the broker"
ok "link connected and enabled; the broker was left running"
python3 tests/hub_probe.py "http://127.0.0.1:$PORT" --token "$TOKEN" --broker ci-server \
    --git /usr/bin/git --cwd "$WORKSPACE"
ok "git --version through the hub and the link"

echo "--- an upgrade keeps the config and the enrollment"
before=$(config_hash)
old=$(readlink "$PREFIX/current")
link_pid=$(prop devagotchi-link.service MainPID)
sudo "$INSTALL" --bin-dir "$BIN" "${COMMON[@]}"
[ "$(readlink "$PREFIX/current")" != "$old" ] || fail "current did not move to the new version"
[ -x "$PREFIX/$old/bridge-broker" ] || fail "the previous version is gone; no rollback target"
[ "$before" = "$(config_hash)" ] || fail "the upgrade changed the config"
wait_for 30 "link restarted on the new version" new_main_pid devagotchi-link.service "$link_pid"
wait_for 30 "link back after the upgrade" link_connected
ok "upgraded to $(readlink "$PREFIX/current"); $old kept for rollback; config and enrollment kept"

echo "--- the link comes back after kill -9"
link_pid=$(prop devagotchi-link.service MainPID)
sudo kill -9 "$link_pid"
wait_for 30 "link restarted under a new pid" new_main_pid devagotchi-link.service "$link_pid"
wait_for 30 "link reconnected" link_connected
ok "restarted by systemd and reconnected"

echo "--- a revoked machine stays down"
"$BIN/devagotchi-hub" revoke --state-dir "$WORK/hub-state" --device ci-server
wait_for 30 "link stopped after the revoke" link_down
status=$(prop devagotchi-link.service ExecMainStatus)
[ "$status" = 2 ] || fail "link exited $status, want 2 (refused)"
restarts=$(prop devagotchi-link.service NRestarts)
sleep 8
link_down || fail "systemd restarted a refused link"
[ "$(prop devagotchi-link.service NRestarts)" = "$restarts" ] || fail "systemd kept restarting a refused link"
ok "exit 2 and not restarted"

echo "--- uninstall"
sudo "$INSTALL" "${COMMON[@]}" --uninstall
[ "$(loginctl show-user "$ACCOUNT" -p Linger --value 2>/dev/null || echo no)" != yes ] || fail "lingering still on"
wait_for 20 "no process left for $ACCOUNT" no_processes_left
ok "services stopped and removed; lingering off"
echo "HOST INSTALL CHECK PASSED"
