#!/bin/sh
# Promote one hub build: staging, smoke, production, rollback.
# Refuses to run without --confirm. See docs/deploy.md.
#
# Production is the hub on Texas (tx-infra-prod, public 23.156.136.173),
# Proxmox CT 110, https://app.devagotchi.com behind Cloudflare.
# Staging is a second container on that same host. Set the variables.
# This script has no host, key, or container id of its own.
#
#   DEVAGOTCHI_STAGING_HOST   user@23.156.136.173   (Proxmox SSH)
#   DEVAGOTCHI_STAGING_LXC    staging container id  (not 110)
#   DEVAGOTCHI_STAGING_URL    https://staging.app.devagotchi.com
#   DEVAGOTCHI_PROD_HOST      user@23.156.136.173
#   DEVAGOTCHI_PROD_LXC       110
#   DEVAGOTCHI_PROD_URL       https://app.devagotchi.com
#
# Live (one hub, no staging container) takes the build from --artifact,
# a local directory or tarball, or an https URL of that tarball. The
# installer is copied in with it. SSH_OPTS, --ssh-opts, and --ssh-key
# are passed to ssh.
set -eu

confirm=0
dry=0
artifact=
ssh_key=
ssh_opts=
while [ $# -gt 0 ]; do
    case $1 in
        --confirm) confirm=1; shift ;;
        --dry-run) dry=1; shift ;;
        --artifact)
            artifact=${2:?--artifact needs a path or URL}
            shift 2
            ;;
        --ssh-key)
            ssh_key=${2:?--ssh-key needs a path}
            shift 2
            ;;
        --ssh-opts)
            ssh_opts=${2:?--ssh-opts needs a string}
            shift 2
            ;;
        *)
            echo "unknown argument: $1" >&2
            echo "usage: $0 --confirm [--dry-run] [--artifact PATH|URL] [--ssh-key FILE] [--ssh-opts WORDS]" >&2
            exit 64
            ;;
    esac
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
script_dir=$(unset CDPATH; cd "$(dirname "$0")" && pwd)

if [ "$dry" != 1 ]; then
    git rev-parse --verify "${DEVAGOTCHI_SHA}^{commit}" >/dev/null
fi
if [ "$target" = "live" ]; then
    echo "live: one hub, no staging container. Snapshot, stage downloads, then install, then smoke."
    if [ -z "$artifact" ]; then
        echo "refusing: live install needs --artifact PATH|URL (the release archive and its .sha256, or a directory of them)" >&2
        exit 64
    fi
else
    : "${DEVAGOTCHI_STAGING_HOST:?user@staging-host}"
    if [ "$dry" != 1 ] && ! git merge-base --is-ancestor "$DEVAGOTCHI_SHA" origin/main; then
        echo "refusing: $DEVAGOTCHI_SHA is not on origin/main" >&2
        exit 1
    fi
fi

# Print or run ssh. SSH_OPTS and --ssh-opts are local words (a port, a key option).
ssh_do() {
    host=$1
    shift
    if [ "$dry" = 1 ]; then
        printf 'dry-run: ssh'
        if [ -n "$ssh_key" ]; then
            printf ' -i %s' "$ssh_key"
        fi
        if [ -n "${SSH_OPTS:-}" ]; then
            printf ' %s' "$SSH_OPTS"
        fi
        if [ -n "$ssh_opts" ]; then
            printf ' %s' "$ssh_opts"
        fi
        printf ' %s --' "$host"
        printf ' %s' "$@"
        printf '\n'
        return 0
    fi
    if [ -n "$ssh_key" ]; then
        # shellcheck disable=SC2086
        ssh -i "$ssh_key" ${SSH_OPTS:-} $ssh_opts "$host" -- "$@"
    else
        # shellcheck disable=SC2086
        ssh ${SSH_OPTS:-} $ssh_opts "$host" -- "$@"
    fi
}

# host, container id (empty when SSH already lands in the guest), then the command.
remote() {
    host=$1
    lxc=$2
    shift 2
    if [ -n "$lxc" ]; then
        ssh_do "$host" pct exec "$lxc" -- "$@"
    else
        ssh_do "$host" "$@"
    fi
}

# Unpack a local tree into a directory on the host or in the container.
copy_tree() {
    host=$1
    lxc=$2
    src=$3
    dst=$4
    if [ "$dry" = 1 ]; then
        echo "dry-run: tar -C $src -czf - . | ssh $host lxc=${lxc:-none} tar -C $dst -xzf -"
        return 0
    fi
    if [ -n "$lxc" ]; then
        ssh_do "$host" pct exec "$lxc" -- mkdir -p "$dst"
        tar -C "$src" -czf - . | ssh_do "$host" pct exec "$lxc" -- tar -C "$dst" -xzf -
    else
        ssh_do "$host" mkdir -p "$dst"
        tar -C "$src" -czf - . | ssh_do "$host" tar -C "$dst" -xzf -
    fi
}

smoke() {
    base=$1
    echo "smoke $base"
    if [ "$dry" = 1 ]; then
        echo "dry-run: curl -fsS $base/healthz"
        echo "dry-run: curl -fsS $base/.well-known/oauth-protected-resource/mcp"
        return 0
    fi
    health=$(curl -fsS "$base/healthz") || return 1
    SMOKE_JSON=$health python3 -c 'import json,os; body=json.loads(os.environ["SMOKE_JSON"]); assert body.get("auth",{}).get("ready") is True, body.get("auth")' || return 1
    meta=$(curl -fsS "$base/.well-known/oauth-protected-resource/mcp") || return 1
    SMOKE_JSON=$meta SMOKE_BASE=$base python3 -c 'import json,os; body=json.loads(os.environ["SMOKE_JSON"]); base=os.environ["SMOKE_BASE"]; assert body["resource"]==base+"/mcp"; assert body["authorization_servers"]==[base]' || return 1
}

# Turn --artifact into a directory that holds the installer and the release files.
stage_artifact() {
    dest=$1
    mkdir -p "$dest"
    case $artifact in
        https://*|http://*)
            if [ "$dry" = 1 ]; then
                echo "dry-run: curl -fsSL -o $dest/artifact.bin $artifact"
            else
                curl -fsSL -o "$dest/artifact.bin" "$artifact"
                case $artifact in
                    *.tar.xz) tar -xJf "$dest/artifact.bin" -C "$dest" ;;
                    *) tar -xzf "$dest/artifact.bin" -C "$dest" ;;
                esac
            fi
            ;;
        *.tar.xz)
            if [ "$dry" = 1 ]; then
                echo "dry-run: tar -xJf $artifact -C $dest"
            else
                tar -xJf "$artifact" -C "$dest"
            fi
            ;;
        *.tar.gz|*.tgz)
            if [ "$dry" = 1 ]; then
                echo "dry-run: tar -xzf $artifact -C $dest"
            else
                tar -xzf "$artifact" -C "$dest"
            fi
            ;;
        *)
            if [ "$dry" = 1 ]; then
                echo "dry-run: cp -a $artifact/. $dest/"
            elif [ ! -d "$artifact" ]; then
                echo "refusing: --artifact $artifact is not a directory, a tarball, or an http(s) URL" >&2
                exit 64
            else
                cp -a "$artifact"/. "$dest"/
            fi
            ;;
    esac
    if [ ! -f "$dest/install-devagotchi.sh" ]; then
        cp "$script_dir/install-devagotchi.sh" "$dest/install-devagotchi.sh"
    fi
    chmod +x "$dest/install-devagotchi.sh" 2>/dev/null || true
    if [ "$dry" != 1 ] && ! find "$dest" -name 'devagotchi-*.tar.xz' -o -name 'devagotchi-*.tar.xz.sha256' | grep -q .; then
        echo "refusing: --artifact has no devagotchi-*.tar.xz (and its .sha256) for install-devagotchi.sh --artifact-dir" >&2
        exit 64
    fi
}

install_release() {
    host=$1
    lxc=$2
    ver=$3
    remote "$host" "$lxc" install-devagotchi.sh --role hub --version "$ver"
}

install_artifact() {
    host=$1
    lxc=$2
    ver=$3
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT INT TERM
    stage_artifact "$work/artifact"
    echo "pushing installer and artifact to $host"
    copy_tree "$host" "$lxc" "$work/artifact" /tmp/devagotchi-promote
    remote "$host" "$lxc" /tmp/devagotchi-promote/install-devagotchi.sh \
        --role hub --version "$ver" --artifact-dir /tmp/devagotchi-promote
}

install_on() {
    host=$1
    lxc=$2
    ver=$3
    if [ -n "$artifact" ]; then
        install_artifact "$host" "$lxc" "$ver"
    else
        install_release "$host" "$lxc" "$ver"
    fi
}

stage_downloads() {
    host=$1
    lxc=$2
    if [ -z "${DEVAGOTCHI_DIST_DIR:-}" ] || [ -z "${DEVAGOTCHI_DOWNLOADS_DIR:-}" ]; then
        return 0
    fi
    echo "staging $DEVAGOTCHI_DIST_DIR into $DEVAGOTCHI_DOWNLOADS_DIR before the hub starts"
    copy_tree "$host" "$lxc" "$DEVAGOTCHI_DIST_DIR" "$DEVAGOTCHI_DOWNLOADS_DIR"
}

rollback_live() {
    if [ -n "$prod_lxc" ]; then
        echo "rolling back snapshot $snap and starting the container" >&2
        remote "$DEVAGOTCHI_PROD_HOST" "" pct rollback "$prod_lxc" "$snap" --start || true
    else
        echo "no container snapshot; reinstalling $previous" >&2
        install_on "$DEVAGOTCHI_PROD_HOST" "" "$previous" || true
    fi
}

if [ "$target" != "live" ]; then
    echo "staging $version from $DEVAGOTCHI_SHA"
    stage_downloads "$DEVAGOTCHI_STAGING_HOST" "$staging_lxc"
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
stage_downloads "$DEVAGOTCHI_PROD_HOST" "$prod_lxc"
if ! install_on "$DEVAGOTCHI_PROD_HOST" "$prod_lxc" "$version"; then
    echo "production install failed; rolling back to $previous" >&2
    if [ "$target" = "live" ]; then
        rollback_live
    else
        install_on "$DEVAGOTCHI_PROD_HOST" "$prod_lxc" "$previous"
    fi
    exit 1
fi
if ! smoke "${DEVAGOTCHI_PROD_URL:?production https URL}"; then
    echo "production smoke failed; rolling back to $previous" >&2
    if [ "$target" = "live" ]; then
        rollback_live
    else
        install_on "$DEVAGOTCHI_PROD_HOST" "$prod_lxc" "$previous"
    fi
    exit 1
fi
if [ "$dry" = 1 ] && [ "$target" = "live" ] && [ -n "$prod_lxc" ]; then
    echo "dry-run: on failure: pct rollback $prod_lxc $snap --start"
fi
echo "promoted $version"
