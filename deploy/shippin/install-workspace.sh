#!/usr/bin/env bash
# Install Devagotchi inside a Shippin.Cloud workspace (Debian 13 / Ubuntu, any user).
# Everything lands under the user's XDG directories, so it survives workspace
# stop/start and rides along in home snapshots; nothing needs root.
#
#   bash install-workspace.sh                 # latest GitHub release
#   DEVAGOTCHI_VERSION=0.1.0 bash install-workspace.sh
#   DEVAGOTCHI_ARTIFACT_DIR=/path/with/tarballs bash install-workspace.sh   # offline / image bake
#
# Idempotent: re-run to move versions; the broker restarts on the new binary.
set -euo pipefail

REPO=${DEVAGOTCHI_REPO:-EonsofStupid/devagotchi}
VERSION=${DEVAGOTCHI_VERSION:-}
ARTIFACT_DIR=${DEVAGOTCHI_ARTIFACT_DIR:-}
TARGET=${DEVAGOTCHI_TARGET:-x86_64-unknown-linux-gnu}
DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}
CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config}
STATE_HOME=${XDG_STATE_HOME:-$HOME/.local/state}
BIN_DIR=${DEVAGOTCHI_BIN_DIR:-$HOME/.local/bin}
WORKSPACE=${DEVAGOTCHI_WORKSPACE:-$HOME}

log() { printf '\n==> %s\n' "$*"; }
need() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 1; }; }
need curl; need tar; need sha256sum; need xz

case "$(uname -m)" in
  x86_64) ;;
  aarch64) TARGET=${DEVAGOTCHI_TARGET:-aarch64-unknown-linux-gnu} ;;
esac

if [ -z "$VERSION" ]; then
  log "Resolving the latest release of $REPO"
  VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -1)
  [ -n "$VERSION" ] || { echo "could not resolve the latest release; set DEVAGOTCHI_VERSION" >&2; exit 1; }
fi
TAG="v$VERSION"
PREFIX="$DATA_HOME/devagotchi/$VERSION"
log "Devagotchi $VERSION for $TARGET → $PREFIX"

tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
fetch() { # name
  if [ -n "$ARTIFACT_DIR" ]; then cp "$ARTIFACT_DIR/$1" "$tmp/$1"; else
    curl -fsSL "https://github.com/$REPO/releases/download/$TAG/$1" -o "$tmp/$1"; fi
}
for app in devagotchi-broker devagotchi-mcp; do
  f="$app-$TARGET.tar.xz"
  fetch "$f"; fetch "$f.sha256"
  (cd "$tmp" && sha256sum -c --quiet "$f.sha256") || { echo "checksum mismatch for $f" >&2; exit 1; }
  if command -v gh >/dev/null 2>&1 && [ -z "$ARTIFACT_DIR" ]; then
    gh attestation verify "$tmp/$f" --repo "$REPO" >/dev/null 2>&1 && echo "    provenance verified: $f" || echo "    (provenance not verified for $f: gh attestation unavailable or unsigned)"
  fi
  mkdir -p "$tmp/x"; tar -C "$tmp/x" -xJf "$tmp/$f"
done
install -d "$PREFIX/bin" "$BIN_DIR"
find "$tmp/x" -type f \( -name bridge-broker -o -name bridge-mcp \) -exec install -m 0755 {} "$PREFIX/bin/" \;
[ -x "$PREFIX/bin/bridge-broker" ] && [ -x "$PREFIX/bin/bridge-mcp" ] || { echo "release tarballs did not contain both binaries" >&2; exit 1; }
ln -sfn "$PREFIX" "$DATA_HOME/devagotchi/current"
ln -sf "$DATA_HOME/devagotchi/current/bin/bridge-broker" "$BIN_DIR/bridge-broker"
ln -sf "$DATA_HOME/devagotchi/current/bin/bridge-mcp" "$BIN_DIR/bridge-mcp"

log "Broker config (grants = the toolchains present in this workspace)"
install -d -m 0700 "$CONFIG_HOME/devagotchi" "$STATE_HOME/devagotchi"
CONF="$CONFIG_HOME/devagotchi/broker.json"
if [ ! -f "$CONF" ]; then
  grants=""
  for exe in git rg node npm python3 cargo rustup go uv; do
    p=$(command -v "$exe" 2>/dev/null || true); [ -n "$p" ] || continue
    p=$(readlink -f "$p"); [ -f "$p" ] || continue
    grants="$grants    \"$p\",\n"
  done
  printf '{\n  "workspace": "%s",\n  "allowed_executables": [\n%s  ],\n  "output_limit_bytes": 262144,\n  "max_operations": 64,\n  "max_concurrent": 4\n}\n' \
    "$WORKSPACE" "$(printf '%b' "$grants" | sed '$ s/,$//')" > "$CONF"
  chmod 0600 "$CONF"
  echo "    wrote $CONF"
else
  echo "    kept existing $CONF"
fi

log "Supervision"
UNIT_DIR="$CONFIG_HOME/systemd/user"
if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
  install -d "$UNIT_DIR"
  cat > "$UNIT_DIR/devagotchi-broker.service" <<EOF
[Unit]
Description=Devagotchi broker (per-user, independently supervised jobs)
Documentation=https://github.com/$REPO
After=default.target

[Service]
Type=simple
ExecStart=$DATA_HOME/devagotchi/current/bin/bridge-broker --state-dir $STATE_HOME/devagotchi --config $CONF
WorkingDirectory=$WORKSPACE
Restart=always
RestartSec=1
Delegate=yes
KillMode=mixed
TimeoutStopSec=30

[Install]
WantedBy=default.target
EOF
  systemctl --user daemon-reload
  systemctl --user enable --now devagotchi-broker.service >/dev/null 2>&1 || systemctl --user restart devagotchi-broker.service
  for i in $(seq 1 50); do systemctl --user is-active --quiet devagotchi-broker.service && break; sleep 0.1; done
  systemctl --user --no-pager --lines=3 status devagotchi-broker.service || true
  if ! loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null | grep -qx yes; then
    echo "    note: linger is off for $(id -un); jobs end with the last login. Root runs: loginctl enable-linger $(id -un)"
  fi
else
  echo "    no systemd --user here: starting the broker unsupervised (restart it yourself after a crash)"
  pkill -u "$(id -un)" -x bridge-broker 2>/dev/null || true
  # setsid -f forks the broker into its own session and returns at once, so it is nobody's
  # child to wait on and keeps no inherited descriptor (stdout may be a pipe when this runs).
  ( cd "$WORKSPACE" && setsid -f "$DATA_HOME/devagotchi/current/bin/bridge-broker" --state-dir "$STATE_HOME/devagotchi" --config "$CONF" \
      < /dev/null > "$STATE_HOME/devagotchi/broker.log" 2>&1 )
  for i in $(seq 1 50); do grep -q 'ready:' "$STATE_HOME/devagotchi/broker.log" 2>/dev/null && break; sleep 0.1; done
  grep -m1 'ready:' "$STATE_HOME/devagotchi/broker.log" || { echo "broker did not report ready; see $STATE_HOME/devagotchi/broker.log" >&2; exit 1; }
fi

log "Done. MCP hosts run: $BIN_DIR/bridge-mcp --socket $STATE_HOME/devagotchi/broker.sock"
