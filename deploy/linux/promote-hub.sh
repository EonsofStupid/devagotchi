#!/bin/sh
# Promote one main-branch hub build: staging, smoke, production, rollback.
# Refuses to run without --confirm and the environment variables below.
# Does not deploy from a pull request commit. See docs/deploy.md.
#
# Production is the hub on Texas (tx-infra-prod, public 23.156.136.173),
# Proxmox CT 110, https://app.devagotchi.com behind Cloudflare.
# Staging is a second container on that same host, its own state directory
# and port, https://staging.app.devagotchi.com. Set the variables. This script
# has no host, key, or container id of its own.
#
#   DEVAGOTCHI_STAGING_HOST   user@23.156.136.173   (Proxmox SSH)
#   DEVAGOTCHI_STAGING_LXC    staging container id  (not 110)
#   DEVAGOTCHI_STAGING_URL    https://staging.app.devagotchi.com
#   DEVAGOTCHI_PROD_HOST      user@23.156.136.173
#   DEVAGOTCHI_PROD_LXC       110
#   DEVAGOTCHI_PROD_URL       https://app.devagotchi.com
set -eu

confirm=0
for arg in "$@"; do
    if [ "$arg" = "--confirm" ]; then
        confirm=1
    else
        echo "unknown argument: $arg" >&2
        echo "usage: DEVAGOTCHI_SHA=… DEVAGOTCHI_STAGING_HOST=… DEVAGOTCHI_PROD_HOST=… $0 --confirm" >&2
        exit 64
    fi
done
if [ "$confirm" != 1 ]; then
    echo "refusing: pass --confirm after reading docs/deploy.md" >&2
    exit 64
fi

: "${DEVAGOTCHI_SHA:?full commit}"
: "${DEVAGOTCHI_PROD_HOST:?user@production-host}"
target=${DEVAGOTCHI_PROMOTE_TARGET:-staged}
version=${DEVAGOTCHI_VERSION:-$DEVAGOTCHI_SHA}
previous=${DEVAGOTCHI_PREVIOUS_VERSION:?version to roll back to}
staging_lxc=${DEVAGOTCHI_STAGING_LXC:-}
prod_lxc=${DEVAGOTCHI_PROD_LXC:-}

git rev-parse --verify "${DEVAGOTCHI_SHA}^{commit}" >/dev/null
if [ "$target" = "live" ]; then
    echo "live: one hub, no staging container. Snapshot first, then install, then smoke."
else
    : "${DEVAGOTCHI_STAGING_HOST:?user@staging-host}"
    if ! git merge-base --is-ancestor "$DEVAGOTCHI_SHA" origin/main; then
        echo "refusing: $DEVAGOTCHI_SHA is not on origin/main" >&2
        exit 1
    fi
fi

# host, container id (empty when SSH already lands in the guest), then the command.
# `--` keeps each word a separate ssh argument, so the local values are not a remote script.
remote() {
    host=$1
    lxc=$2
    shift 2
    if [ -n "$lxc" ]; then
        ssh "$host" -- pct exec "$lxc" -- "$@"
    else
        ssh "$host" -- "$@"
    fi
}

smoke() {
    base=$1
    echo "smoke $base"
    health=$(curl -fsS "$base/healthz") || return 1
    SMOKE_JSON=$health python3 -c 'import json,os; body=json.loads(os.environ["SMOKE_JSON"]); assert body.get("auth",{}).get("ready") is True, body.get("auth")' || return 1
    meta=$(curl -fsS "$base/.well-known/oauth-protected-resource/mcp") || return 1
    SMOKE_JSON=$meta SMOKE_BASE=$base python3 -c 'import json,os; body=json.loads(os.environ["SMOKE_JSON"]); base=os.environ["SMOKE_BASE"]; assert body["resource"]==base+"/mcp"; assert body["authorization_servers"]==[base]' || return 1
}

install_on() {
    host=$1
    lxc=$2
    ver=$3
    remote "$host" "$lxc" install-devagotchi.sh --role hub --version "$ver"
}

if [ "$target" != "live" ]; then
    echo "staging $version from $DEVAGOTCHI_SHA"
    if ! install_on "$DEVAGOTCHI_STAGING_HOST" "$staging_lxc" "$version"; then
        echo "staging install failed; rolling back to $previous" >&2
        install_on "$DEVAGOTCHI_STAGING_HOST" "$staging_lxc" "$previous"
        exit 1
    fi
    if ! smoke "${DEVAGOTCHI_STAGING_URL:?staging https URL}"; then
        echo "staging smoke failed; rolling back to $previous" >&2
        install_on "$DEVAGOTCHI_STAGING_HOST" "$staging_lxc" "$previous"
        exit 1
    fi
fi

snap=devagotchi-pre-$version
if [ "$target" = "live" ] && [ -n "$prod_lxc" ]; then
    echo "snapshot $snap on $prod_lxc"
    remote "$DEVAGOTCHI_PROD_HOST" "" pct snapshot "$prod_lxc" "$snap" || {
        echo "refusing: snapshot failed" >&2
        exit 1
    }
fi

echo "production $version"
if ! install_on "$DEVAGOTCHI_PROD_HOST" "$prod_lxc" "$version"; then
    echo "production install failed; rolling back to $previous" >&2
    if [ "$target" = "live" ] && [ -n "$prod_lxc" ]; then
        remote "$DEVAGOTCHI_PROD_HOST" "" pct rollback "$prod_lxc" "$snap" || true
    fi
    install_on "$DEVAGOTCHI_PROD_HOST" "$prod_lxc" "$previous"
    exit 1
fi
if ! smoke "${DEVAGOTCHI_PROD_URL:?production https URL}"; then
    echo "production smoke failed; rolling back to $previous" >&2
    if [ "$target" = "live" ] && [ -n "$prod_lxc" ]; then
        remote "$DEVAGOTCHI_PROD_HOST" "" pct rollback "$prod_lxc" "$snap" || true
    fi
    install_on "$DEVAGOTCHI_PROD_HOST" "$prod_lxc" "$previous"
    exit 1
fi
if [ -n "${DEVAGOTCHI_DIST_DIR:-}" ] && [ -n "${DEVAGOTCHI_DOWNLOADS_DIR:-}" ]; then
    echo "copying $DEVAGOTCHI_DIST_DIR into the hub downloads directory so /join serves this build"
    tar -C "$DEVAGOTCHI_DIST_DIR" -czf /tmp/devagotchi-dist.tgz .
    scp /tmp/devagotchi-dist.tgz "$DEVAGOTCHI_PROD_HOST:/tmp/devagotchi-dist.tgz"
    if [ -n "$prod_lxc" ]; then
        remote "$DEVAGOTCHI_PROD_HOST" "" pct push "$prod_lxc" /tmp/devagotchi-dist.tgz /tmp/devagotchi-dist.tgz
        remote "$DEVAGOTCHI_PROD_HOST" "$prod_lxc" tar -C "$DEVAGOTCHI_DOWNLOADS_DIR" -xzf /tmp/devagotchi-dist.tgz
    else
        remote "$DEVAGOTCHI_PROD_HOST" "" tar -C "$DEVAGOTCHI_DOWNLOADS_DIR" -xzf /tmp/devagotchi-dist.tgz
    fi
fi
echo "promoted $version"
