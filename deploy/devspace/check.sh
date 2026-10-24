#!/usr/bin/env bash
# What this devspace tenant has. Run as the devspace user after provision.sh
# (installed as /devspace/bin/devspace-check). Exit 1 if anything expected is missing.
set -uo pipefail
DEVSPACE_ROOT=${DEVSPACE_ROOT:-/devspace}
. "$DEVSPACE_ROOT/env.sh"
status=0

check() { # name command...
  local name=$1 out; shift
  if out=$("$@" 2>&1); then
    printf '  ok    %-18s %s\n' "$name" "${out%%$'\n'*}"
  else
    printf '  FAIL  %-18s %s\n' "$name" "${out%%$'\n'*}"; status=1
  fi
}

echo "devspace $DEVSPACE_ROOT as $(id -un) on $(. /etc/os-release; echo "$PRETTY_NAME") $(uname -r)"
echo "toolchains:"
check rustup rustup --version
check go go version
check uv uv --version
check python3.12 python3.12 --version
check python3.13 python3.13 --version
check node node --version
check npm npm --version
check desktop-commander command -v desktop-commander
check claude claude --version
check git git --version
check rg rg --version
check tmux tmux -V

echo "layout:"
for d in repos state config toolchains receipts bin; do
  if [ -d "$DEVSPACE_ROOT/$d" ]; then printf '  ok    %s\n' "$d"; else printf '  FAIL  %s missing\n' "$d"; status=1; fi
done
[ -f "$DEVSPACE_ROOT/config/devagotchi/broker.json" ] && echo "  ok    config/devagotchi/broker.json" || { echo "  FAIL  config/devagotchi/broker.json missing"; status=1; }
[ -f "$HOME/.config/systemd/user/devagotchi-broker.service" ] && echo "  ok    devagotchi-broker.service installed" || { echo "  FAIL  devagotchi-broker.service missing"; status=1; }

echo "home (everything a toolchain writes should land under $DEVSPACE_ROOT):"
stray=$(find "$HOME" -mindepth 1 -maxdepth 1 ! -name '.bash*' ! -name '.profile' ! -name '.config' ! -name '.ssh' ! -name '.sudo_as_admin_successful' ! -name '.claude*' 2>/dev/null)
if [ -z "$stray" ]; then echo "  ok    nothing stray in \$HOME"; else echo "  note  stray entries in \$HOME:"; echo "$stray" | sed 's/^/        /'; fi

echo "services:"
if [ -d /run/systemd/system ]; then
  check linger sh -c "loginctl show-user '$(id -un)' -p Linger --value | grep -qx yes && echo yes"
  if [ -x "$DEVSPACE_ROOT/bin/devagotchi" ]; then
    check devagotchi-broker systemctl --user is-active devagotchi-broker
  else
    echo "  note  broker not built yet (README: Build and start the broker)"
  fi
else
  echo "  note  no running systemd here; linger and the broker unit are checked on the real tenant"
fi
exit $status
