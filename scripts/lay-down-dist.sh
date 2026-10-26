#!/usr/bin/env bash
# Lay down the directory a hub hands to joining machines (`"downloads"` in hub.json): GitHub's
# build of one commit on main for every target, and that commit's installers.
#
#   scripts/lay-down-dist.sh COMMIT OUT_DIR
#
# Each archive comes through fetch-main-build.sh (sha256 against its .sha256 and `gh
# attestation verify`). The installers are read from COMMIT itself. Writes OUT_DIR/VERSION
# (main-<short>) and OUT_DIR/SHA256SUMS; the hub hashes every listed file again at start.
# Needs gh signed in with read access to the repository's Actions.
set -euo pipefail
commit=${1:?usage: lay-down-dist.sh COMMIT OUT_DIR}
out=${2:?usage: lay-down-dist.sh COMMIT OUT_DIR}
repo=${DEVAGOTCHI_REPO:-EonsofStupid/devagotchi}
here=$(cd "$(dirname "$0")" && pwd)
sha=$(gh api "repos/$repo/commits/$commit" --jq .sha)
mkdir -p "$out"
for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
    got=$(mktemp -d)
    "$here/fetch-main-build.sh" "$sha" "$target" "$got" >/dev/null
    for archive in "$got"/devagotchi-"$target".tar.xz "$got"/devagotchi-"$target".zip; do
        [ -e "$archive" ] && cp "$archive" "$out/"
    done
    rm -rf "$got"
done
for installer in deploy/linux/install-devagotchi.sh deploy/windows/install-devagotchi.ps1; do
    gh api "repos/$repo/contents/$installer?ref=$sha" -H 'Accept: application/vnd.github.raw' \
        > "$out/$(basename "$installer")"
done
echo "main-${sha:0:7}" > "$out/VERSION"
(cd "$out" && sha256sum -- devagotchi-* install-devagotchi.* > SHA256SUMS)
echo "lay-down-dist: $(wc -l < "$out/SHA256SUMS") files for main-${sha:0:7} in $out" >&2
cat "$out/SHA256SUMS" >&2
