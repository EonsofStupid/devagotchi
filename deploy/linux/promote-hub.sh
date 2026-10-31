#!/bin/sh
# Promote one main-branch hub build: staging, smoke, production, rollback.
# Refuses to run without --confirm and the environment variables below.
# Does not deploy from a pull request commit. See docs/deploy.md.
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

: "${DEVAGOTCHI_SHA:?full commit that is on origin/main}"
: "${DEVAGOTCHI_STAGING_HOST:?user@staging-host}"
: "${DEVAGOTCHI_PROD_HOST:?user@production-host}"
version=${DEVAGOTCHI_VERSION:-$DEVAGOTCHI_SHA}
previous=${DEVAGOTCHI_PREVIOUS_VERSION:?version to roll back to}

git rev-parse --verify "${DEVAGOTCHI_SHA}^{commit}" >/dev/null
if ! git merge-base --is-ancestor "$DEVAGOTCHI_SHA" origin/main; then
    echo "refusing: $DEVAGOTCHI_SHA is not on origin/main" >&2
    exit 1
fi

remote() {
    host=$1
    shift
    if [ -n "${PROMOTE_LXC:-}" ]; then
        ssh "$host" pct exec "$PROMOTE_LXC" -- "$@"
    else
        ssh "$host" "$@"
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
    ver=$2
    remote "$host" install-devagotchi.sh --role hub --version "$ver"
}

echo "staging $version from $DEVAGOTCHI_SHA"
if ! install_on "$DEVAGOTCHI_STAGING_HOST" "$version"; then
    echo "staging install failed; rolling back to $previous" >&2
    install_on "$DEVAGOTCHI_STAGING_HOST" "$previous"
    exit 1
fi
if ! SMOKE_BASE=${DEVAGOTCHI_STAGING_URL:?staging https URL} smoke "$DEVAGOTCHI_STAGING_URL"; then
    echo "staging smoke failed; rolling back to $previous" >&2
    install_on "$DEVAGOTCHI_STAGING_HOST" "$previous"
    exit 1
fi

echo "production $version"
export PROMOTE_LXC=${DEVAGOTCHI_PROD_LXC:-}
if ! install_on "$DEVAGOTCHI_PROD_HOST" "$version"; then
    echo "production install failed; rolling back to $previous" >&2
    install_on "$DEVAGOTCHI_PROD_HOST" "$previous"
    exit 1
fi
if ! smoke "${DEVAGOTCHI_PROD_URL:?production https URL}"; then
    echo "production smoke failed; rolling back to $previous" >&2
    install_on "$DEVAGOTCHI_PROD_HOST" "$previous"
    exit 1
fi
echo "promoted $version"
