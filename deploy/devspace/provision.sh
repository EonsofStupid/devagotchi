#!/usr/bin/env bash
# Provision a Devagotchi devspace tenant on Ubuntu 24.04 (Incus system container
# or VM with systemd). Idempotent: re-run after changing a version below.
#
# Everything the tenant owns lives under one root so the environment is confined,
# snapshot-able, and portable:
#
#   /devspace/
#     repos/        git clones (devagotchi, agent-skills, ...)
#     state/        XDG_STATE_HOME: broker journal, logs
#     config/       XDG_CONFIG_HOME: broker.json, host configs
#     toolchains/   rustup, cargo, go, uv + pythons, fnm + node
#     receipts/     verification and benchmark run directories
#     bin/          symlinks to the built bridge-broker / bridge-mcp
#     env.sh        the exports every shell and systemd user service load
#
# Run as root once:  DEVSPACE_USER=hades bash provision.sh
set -euo pipefail

DEVSPACE_ROOT=${DEVSPACE_ROOT:-/devspace}
DEVSPACE_USER=${DEVSPACE_USER:-hades}
DEVSPACE_SUDO_NOPASSWD=${DEVSPACE_SUDO_NOPASSWD:-1}
NODE_MAJOR=${NODE_MAJOR:-22}                 # Desktop Commander's remote agent needs >= 22.12
PYTHON_VERSIONS=${PYTHON_VERSIONS:-"3.12 3.13"}
GO_VERSION=${GO_VERSION:-}                   # empty = current stable from go.dev
INSTALL_CLAUDE_CODE=${INSTALL_CLAUDE_CODE:-1}
INSTALL_CARGO_TOOLS=${INSTALL_CARGO_TOOLS:-0} # cargo-nextest, cargo-deny (slow; tooling PR 3)

log() { printf '\n==> %s\n' "$*"; }
need_root() { [ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }; }
need_root

. /etc/os-release
if [ "${ID:-}" != "ubuntu" ] || [ "${VERSION_ID:-}" != "24.04" ]; then
  echo "This script targets Ubuntu 24.04 (got ${PRETTY_NAME:-unknown}); edit it before using another base." >&2
  exit 1
fi

log "System packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q --no-install-recommends \
  build-essential pkg-config libssl-dev \
  git curl ca-certificates gnupg unzip jq ripgrep fd-find tmux sqlite3 \
  openssh-server dbus-user-session libpam-systemd sudo
apt-get autoremove -y -q

log "User ${DEVSPACE_USER}"
if ! id -u "$DEVSPACE_USER" >/dev/null 2>&1; then
  useradd --create-home --shell /bin/bash --groups sudo "$DEVSPACE_USER"
fi
if [ "$DEVSPACE_SUDO_NOPASSWD" = "1" ]; then
  echo "$DEVSPACE_USER ALL=(ALL) NOPASSWD:ALL" > "/etc/sudoers.d/90-$DEVSPACE_USER"
  chmod 0440 "/etc/sudoers.d/90-$DEVSPACE_USER"
fi
# Jobs must outlive logout: only lingering user services survive the last session ending.
if [ -d /run/systemd/system ]; then
  loginctl enable-linger "$DEVSPACE_USER"
else
  echo "    (no running systemd: skipping enable-linger; run it on the real tenant)"
fi

log "Layout under ${DEVSPACE_ROOT}"
for d in repos state config toolchains receipts bin; do
  install -d -o "$DEVSPACE_USER" -g "$DEVSPACE_USER" -m 0750 "$DEVSPACE_ROOT/$d"
done
chown "$DEVSPACE_USER:$DEVSPACE_USER" "$DEVSPACE_ROOT"
chmod 0755 "$DEVSPACE_ROOT"

log "Environment"
# The same exports for login shells and for systemd --user services.
cat > "$DEVSPACE_ROOT/env.sh" <<EOF
# Devagotchi devspace environment. Sourced by /etc/profile.d and systemd environment.d.
export DEVSPACE_ROOT=$DEVSPACE_ROOT
export XDG_STATE_HOME=$DEVSPACE_ROOT/state
export XDG_CONFIG_HOME=$DEVSPACE_ROOT/config
export RUSTUP_HOME=$DEVSPACE_ROOT/toolchains/rustup
export CARGO_HOME=$DEVSPACE_ROOT/toolchains/cargo
export GOROOT=$DEVSPACE_ROOT/toolchains/go
export GOPATH=$DEVSPACE_ROOT/toolchains/gopath
export GOCACHE=$DEVSPACE_ROOT/toolchains/go-cache
export GOTOOLCHAIN=local
export UV_INSTALL_DIR=$DEVSPACE_ROOT/toolchains/uv
export UV_PYTHON_INSTALL_DIR=$DEVSPACE_ROOT/toolchains/python
export UV_PYTHON_BIN_DIR=$DEVSPACE_ROOT/toolchains/python-bin
export UV_TOOL_DIR=$DEVSPACE_ROOT/toolchains/uv-tools
export UV_TOOL_BIN_DIR=$DEVSPACE_ROOT/toolchains/python-bin
export UV_CACHE_DIR=$DEVSPACE_ROOT/toolchains/uv-cache
export FNM_DIR=$DEVSPACE_ROOT/toolchains/fnm
export NPM_CONFIG_PREFIX=$DEVSPACE_ROOT/toolchains/npm
export NPM_CONFIG_CACHE=$DEVSPACE_ROOT/toolchains/npm-cache
# uv and Node ship their own root stores; point both at the system store too, so a tenant
# behind TLS inspection (corporate CA under /usr/local/share/ca-certificates) behaves like curl and apt.
export UV_SYSTEM_CERTS=1
export NODE_EXTRA_CA_CERTS=/etc/ssl/certs/ca-certificates.crt
export PATH=$DEVSPACE_ROOT/bin:\$CARGO_HOME/bin:\$GOROOT/bin:\$GOPATH/bin:\$UV_INSTALL_DIR:\$UV_PYTHON_BIN_DIR:\$FNM_DIR/bin:\$FNM_DIR/aliases/default/bin:\$NPM_CONFIG_PREFIX/bin:\$PATH
EOF
chown "$DEVSPACE_USER:$DEVSPACE_USER" "$DEVSPACE_ROOT/env.sh"
ln -sf "$DEVSPACE_ROOT/env.sh" /etc/profile.d/devspace.sh

# systemd --user services do not read profile.d; environment.d does the same job for them.
# Evaluate env.sh in an empty environment so every value is written out fully resolved.
USER_HOME=$(getent passwd "$DEVSPACE_USER" | cut -d: -f6)
# install -d only chowns the leaf; make the intermediate directories the user's too, or the
# user cannot create ~/.config/<app> later (found by the Shippin workspace installer).
install -d -o "$DEVSPACE_USER" -g "$DEVSPACE_USER" "$USER_HOME/.config" "$USER_HOME/.config/environment.d" \
  "$USER_HOME/.config/systemd" "$USER_HOME/.config/systemd/user" "$USER_HOME/.local" "$USER_HOME/.local/bin"
env -i HOME="$USER_HOME" PATH=/usr/local/bin:/usr/bin:/bin bash --noprofile --norc -c \
  ". '$DEVSPACE_ROOT/env.sh'; for v in \$(grep -oE '^export [A-Z_]+' '$DEVSPACE_ROOT/env.sh' | cut -d' ' -f2); do printf '%s=%s\n' \"\$v\" \"\${!v}\"; done" \
  > "$USER_HOME/.config/environment.d/10-devspace.conf"
chown "$DEVSPACE_USER:$DEVSPACE_USER" "$USER_HOME/.config/environment.d/10-devspace.conf"

as_user() { sudo -u "$DEVSPACE_USER" -H bash -lc "set -euo pipefail; . $DEVSPACE_ROOT/env.sh; $*"; }

log "Rust (rustup; the repo's rust-toolchain.toml selects the compiler on first build)"
if ! as_user "command -v rustup >/dev/null"; then
  as_user "curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --default-toolchain none --profile minimal"
fi
if [ "$INSTALL_CARGO_TOOLS" = "1" ]; then
  as_user "rustup toolchain install stable --profile minimal && cargo +stable install --locked cargo-nextest cargo-deny"
fi

log "Go"
if [ -z "$GO_VERSION" ]; then
  GO_VERSION=$(curl -fsSL 'https://go.dev/VERSION?m=text' | head -1 | sed 's/^go//')
fi
if ! as_user "go version 2>/dev/null | grep -q 'go$GO_VERSION '"; then
  arch=$(dpkg --print-architecture); case "$arch" in amd64) goarch=amd64;; arm64) goarch=arm64;; *) echo "unsupported arch $arch" >&2; exit 1;; esac
  tmp=$(mktemp -d); curl -fsSL "https://go.dev/dl/go${GO_VERSION}.linux-${goarch}.tar.gz" -o "$tmp/go.tgz"
  rm -rf "$DEVSPACE_ROOT/toolchains/go"; tar -C "$DEVSPACE_ROOT/toolchains" -xzf "$tmp/go.tgz"; rm -rf "$tmp"
  chown -R "$DEVSPACE_USER:$DEVSPACE_USER" "$DEVSPACE_ROOT/toolchains/go"
  install -d -o "$DEVSPACE_USER" -g "$DEVSPACE_USER" "$DEVSPACE_ROOT/toolchains/gopath"
fi

log "Python via uv (${PYTHON_VERSIONS})"
if ! as_user "command -v uv >/dev/null"; then
  as_user "curl -LsSf https://astral.sh/uv/install.sh | UV_NO_MODIFY_PATH=1 sh"
fi
as_user "uv python install $PYTHON_VERSIONS && uv python list --only-installed"

log "Node ${NODE_MAJOR} via fnm (Desktop Commander remote agent, website tooling, contract generator)"
if ! as_user "command -v fnm >/dev/null"; then
  as_user "curl -fsSL https://fnm.vercel.app/install | bash -s -- --install-dir $DEVSPACE_ROOT/toolchains/fnm/bin --skip-shell"
fi
as_user "fnm install $NODE_MAJOR && fnm default $NODE_MAJOR && node --version"
# --ignore-scripts: a global npm install lays down the macOS-only fsevents and then tries to
# node-gyp it on Linux (npm quirk); Desktop Commander's own postinstall is only telemetry and a
# ripgrep check, and its ripgrep comes as a platform package (@vscode/ripgrep-linux-x64).
as_user "npm install -g --ignore-scripts --no-audit --no-fund @wonderwhy-er/desktop-commander@latest >/dev/null && echo 'desktop-commander installed'"

if [ "$INSTALL_CLAUDE_CODE" = "1" ]; then
  log "Claude Code (signed apt repository, stable channel)"
  install -d -m 0755 /etc/apt/keyrings
  curl -fsSL https://downloads.claude.ai/keys/claude-code.asc -o /etc/apt/keyrings/claude-code.asc
  fp=$(gpg --show-keys --with-colons /etc/apt/keyrings/claude-code.asc | awk -F: '/^fpr/ {print $10; exit}')
  if [ "$fp" != "31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE" ]; then
    echo "Claude Code signing key fingerprint mismatch: $fp" >&2; exit 1
  fi
  echo "deb [signed-by=/etc/apt/keyrings/claude-code.asc] https://downloads.claude.ai/claude-code/apt/stable stable main" \
    > /etc/apt/sources.list.d/claude-code.list
  apt-get update -q && apt-get install -y -q claude-code
fi

log "Broker config and systemd user unit"
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
install -d -o "$DEVSPACE_USER" -g "$DEVSPACE_USER" -m 0700 "$DEVSPACE_ROOT/config/devagotchi" "$DEVSPACE_ROOT/state/devagotchi"
if [ ! -f "$DEVSPACE_ROOT/config/devagotchi/broker.json" ]; then
  sed "s|/devspace|$DEVSPACE_ROOT|g" "$HERE/broker.devspace.json" > "$DEVSPACE_ROOT/config/devagotchi/broker.json"
  chown "$DEVSPACE_USER:$DEVSPACE_USER" "$DEVSPACE_ROOT/config/devagotchi/broker.json"; chmod 0600 "$DEVSPACE_ROOT/config/devagotchi/broker.json"
fi
sed "s|/devspace|$DEVSPACE_ROOT|g" "$HERE/devagotchi-broker.service" > "$USER_HOME/.config/systemd/user/devagotchi-broker.service"
chown "$DEVSPACE_USER:$DEVSPACE_USER" "$USER_HOME/.config/systemd/user/devagotchi-broker.service"
if [ -d /run/systemd/system ]; then
  as_user "systemctl --user daemon-reload && systemctl --user enable devagotchi-broker.service" || echo "    (user manager not reachable yet; the unit is installed and enables on first login)"
else
  echo "    (no running systemd: unit installed to ~/.config/systemd/user, enable it on the real tenant)"
fi

if [ -f "$HERE/check.sh" ]; then
  install -o "$DEVSPACE_USER" -g "$DEVSPACE_USER" -m 0755 "$HERE/check.sh" "$DEVSPACE_ROOT/bin/devspace-check"
fi

log "Done. As $DEVSPACE_USER: devspace-check. Then deploy/devspace/README.md → 'Build and start the broker' and 'Connect remotely'."
