#!/usr/bin/env bash
# CI driver for install-devagotchi.sh on a real systemd host (.github/workflows/ci.yml,
# host-install job). Needs passwordless sudo and the debug build in target/debug.
#
# Checks, in order: install from the build as a dedicated account with lingering; the
# services run as that account from root-owned binaries; a real operation through the
# broker; the broker comes back after kill -9; the link joins a hub with a code piped
# through the installer (never on a command line) and a real operation runs through the
# hub and the link; an upgrade keeps the config and the enrollment; the link comes back
# after kill -9; a revoke makes the link exit 2 and systemd leaves it down; the hub role on the
# same host (a board initialised, a person and an agent with minted tokens, Caddy in front of
# the loopback listener, the board worked over MCP through Caddy, a re-run that only mints a
# token and leaves the hub running); uninstall stops everything. Exit 0 only when every step
# holds.
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
hub_ready() { grep -q 'devagotchi hub ready:' "$WORK/hub.log"; }
no_processes_left() { ! pgrep -u "$ACCOUNT" -f 'devagotchi (broker|link|hub) '; }
hub_healthy() { curl -fsS -m 2 "http://127.0.0.1:$1/healthz" >/dev/null; }
hub_config_hash() { sudo sha256sum "$HOME_DIR/.config/devagotchi/hub.json" | cut -d' ' -f1; }
link_connected() {
    curl -s "http://127.0.0.1:$PORT/healthz" | python3 -c \
        'import json,sys; b=[x for x in json.load(sys.stdin)["brokers"] if x["name"]=="ci-server"]; sys.exit(0 if b and b[0].get("connected") else 1)'
}
config_hash() { sudo sha256sum "$HOME_DIR/.config/devagotchi/broker.json" | cut -d' ' -f1; }

sudo install -d -m 0755 "$PROBES"
sudo install -m 0644 tests/adapter_probe.py tests/hub_probe.py "$PROBES/"

echo "--- the release build this machine takes"
want=$(rustc -vV | sed -n 's/^host: //p')
got=$("$INSTALL" --print-target)
[ "$got" = "$want" ] || fail "the installer picks $got; this machine's toolchain builds $want"
ok "the installer picks $got, the same target this runner builds"

echo "--- install from the build"
sudo "$INSTALL" --bin-dir "$BIN" "${COMMON[@]}" --grant /usr/bin/git
broker_up || fail "broker not up"
[ "$(uctl is-enabled devagotchi-broker.service)" = enabled ] || fail "broker not enabled"
[ "$(loginctl show-user "$ACCOUNT" -p Linger --value)" = yes ] || fail "lingering is off"
ok "broker active and enabled; lingering on (starts at boot with nobody logged in)"
pid=$(prop devagotchi-broker.service MainPID)
[ "$(ps -o user= -p "$pid" | tr -d ' ')" = "$ACCOUNT" ] || fail "broker pid $pid does not run as $ACCOUNT"
[ "$(stat -c '%U %a' "$PREFIX/current/devagotchi")" = "root 755" ] || fail "the binary is not root-owned 0755"
as_account test ! -w "$PREFIX/current/devagotchi" || fail "$ACCOUNT can write its own binary"
[ "$(sudo stat -c '%U %a' "$HOME_DIR/.config/devagotchi/broker.json")" = "$ACCOUNT 600" ] || fail "config is not private"
ok "runs as $ACCOUNT from root-owned binaries; config 0600"
link_down || fail "link runs before enrollment"
ok "no link before enrollment"

echo "--- a real operation through the broker"
as_account python3 "$PROBES/adapter_probe.py" "$SOCKET" --adapter "$PREFIX/current/devagotchi mcp" \
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
"$BIN/devagotchi" hub serve --state-dir "$WORK/hub-state" --config "$WORK/hub.json" 2> "$WORK/hub.log" &
HUB_PID=$!
wait_for 20 "hub ready" hub_ready
broker_pid=$(prop devagotchi-broker.service MainPID)
code=$("$BIN/devagotchi" hub enroll --state-dir "$WORK/hub-state" --device ci-server 2>/dev/null)
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
[ -x "$PREFIX/$old/devagotchi" ] || fail "the previous version is gone; no rollback target"
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
"$BIN/devagotchi" hub revoke --state-dir "$WORK/hub-state" --device ci-server
wait_for 30 "link stopped after the revoke" link_down
status=$(prop devagotchi-link.service ExecMainStatus)
[ "$status" = 2 ] || fail "link exited $status, want 2 (refused)"
restarts=$(prop devagotchi-link.service NRestarts)
sleep 8
link_down || fail "systemd restarted a refused link"
[ "$(prop devagotchi-link.service NRestarts)" = "$restarts" ] || fail "systemd kept restarting a refused link"
ok "exit 2 and not restarted"

echo "--- the hub role on the same host, behind Caddy"
HPORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
CPORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
broker_pid=$(prop devagotchi-broker.service MainPID)
# Tokens are printed once; keep them out of the log.
out=$(sudo "$INSTALL" "${COMMON[@]}" --role hub --public-url "http://127.0.0.1:$CPORT" --listen "127.0.0.1:$HPORT" \
    --board ci-project --person ci-op --agent ci-agent --caddy)
printf '%s\n' "$out" | sed -E 's/^(    (ci-op|ci-agent)): [0-9a-f]{48}$/\1: <token>/'
op_token=$(printf '%s\n' "$out" | sed -nE 's/^    ci-op: ([0-9a-f]{48})$/\1/p')
agent_token=$(printf '%s\n' "$out" | sed -nE 's/^    ci-agent: ([0-9a-f]{48})$/\1/p')
{ [ ${#op_token} = 48 ] && [ ${#agent_token} = 48 ]; } || fail "the installer did not print both tokens"
[ "$(uctl is-enabled devagotchi-hub.service)" = enabled ] || fail "hub not enabled"
[ "$(sudo stat -c '%U %a' "$HOME_DIR/.config/devagotchi/hub.json")" = "$ACCOUNT 600" ] || fail "hub config is not private"
[ "$(sudo stat -c '%U %a' "$HOME_DIR/.config/devagotchi/hub-tokens.json")" = "$ACCOUNT 600" ] || fail "tokens file is not private"
[ "$(prop devagotchi-broker.service MainPID)" = "$broker_pid" ] || fail "installing the hub restarted the broker"
sudo test -f "$HOME_DIR/projects/ci-project/.devagotchi/board.sqlite3" || fail "the board was not initialised"
sudo grep -q 'devagotchi-board:start' "$HOME_DIR/projects/ci-project/AGENTS.md" || fail "AGENTS.md block missing"
hub_healthy "$HPORT" || fail "hub not healthy on its loopback port"
wait_for 20 "Caddy answers in front of the hub" hub_healthy "$CPORT"
ok "hub active and enabled from the installed version, config and tokens 0600, board initialised, Caddy at :$CPORT -> :$HPORT"
python3 - "http://127.0.0.1:$CPORT" "$op_token" "$agent_token" <<'PY'
import json, sys, urllib.request
base, op, agent = sys.argv[1:4]
class C:
    def __init__(self, token):
        self.token, self.sid, self.n = token, None, 0
        self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "ci", "version": "0"}})
        self.post({"jsonrpc": "2.0", "method": "notifications/initialized"})
    def post(self, body):
        req = urllib.request.Request(base + "/mcp", data=json.dumps(body).encode(), method="POST")
        for k, v in {"Accept": "application/json, text/event-stream", "Content-Type": "application/json",
                     "Authorization": "Bearer " + self.token, "MCP-Protocol-Version": "2025-06-18"}.items():
            req.add_header(k, v)
        if self.sid: req.add_header("Mcp-Session-Id", self.sid)
        with urllib.request.urlopen(req, timeout=30) as r:
            self.sid = self.sid or r.headers.get("Mcp-Session-Id"); text = r.read().decode(); ct = r.headers.get("Content-Type", "")
        if ct.startswith("text/event-stream"):
            text = [l[5:].strip() for l in text.splitlines() if l.startswith("data:")][-1]
        return json.loads(text) if text else {}
    def rpc(self, method, params):
        self.n += 1
        r = self.post({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params})
        assert "error" not in r, r
        return r["result"]
    def call(self, tool, args):
        r = self.rpc("tools/call", {"name": tool, "arguments": args})
        return r.get("isError", False), r.get("structuredContent")
person, bot = C(op), C(agent)
_, me = person.call("whoami", {}); assert me["human"] and me["boards"] == ["ci-project"], me
_, me = bot.call("whoami", {}); assert not me["human"] and me["boards"] == ["ci-project"], me
err, t = person.call("board_create", {"project": "ci-project", "title": "Prove the hub role through Caddy"}); assert not err, t
err, lease = bot.call("board_claim", {"project": "ci-project"}); assert not err and lease["claimed"] and lease["lease"]["ticket"]["id"] == t["id"], lease
err, done = bot.call("board_complete", {"project": "ci-project", "id": t["id"], "summary": "claimed and completed over MCP through Caddy"}); assert not err and done["state"] == "review", done
err, refused = bot.call("board_force_release", {"project": "ci-project", "id": t["id"], "reason": "no"}); assert err and refused["code"] == "humans_only", refused
print("ok   a person created, the agent claimed and completed, force-release refused to the agent: all through Caddy")
PY
echo "--- a hub re-run only mints a token"
before=$(hub_config_hash)
hub_pid=$(prop devagotchi-hub.service MainPID)
out=$(sudo "$INSTALL" "${COMMON[@]}" --role hub --agent ci-agent-2)
printf '%s\n' "$out" | sed -E 's/^(    ci-agent-2): [0-9a-f]{48}$/\1: <token>/'
token2=$(printf '%s\n' "$out" | sed -nE 's/^    ci-agent-2: ([0-9a-f]{48})$/\1/p')
[ ${#token2} = 48 ] || fail "no token for ci-agent-2"
[ "$before" = "$(hub_config_hash)" ] || fail "the re-run changed hub.json"
[ "$(prop devagotchi-hub.service MainPID)" = "$hub_pid" ] || fail "the re-run restarted the hub"
printf '%s\n' "$out" | grep -q '"subject": "ci-agent-2"' || fail "the re-run did not print the grant line to add"
ok "token minted, config kept, hub left running, grant line printed"

echo "--- uninstall"
sudo "$INSTALL" "${COMMON[@]}" --uninstall
[ "$(loginctl show-user "$ACCOUNT" -p Linger --value 2>/dev/null || echo no)" != yes ] || fail "lingering still on"
wait_for 20 "no process left for $ACCOUNT" no_processes_left
ok "services stopped and removed; lingering off"
echo "HOST INSTALL CHECK PASSED"
