#!/usr/bin/env bash
# Fetch GitHub's build of one commit on main and prove it before anything installs it.
#
#   scripts/fetch-main-build.sh COMMIT [TARGET] [OUT_DIR]
#
# Finds the successful release-workflow run for COMMIT on main, downloads that run's
# artifact for TARGET (default x86_64-unknown-linux-gnu), checks each archive against its
# .sha256 and verifies its GitHub build attestation (gh attestation verify). Prints OUT_DIR,
# ready for: install-devagotchi.sh --version main-<short> --artifact-dir OUT_DIR
# Needs gh signed in with read access to the repository's Actions.
set -euo pipefail
commit=${1:?usage: fetch-main-build.sh COMMIT [TARGET] [OUT_DIR]}
target=${2:-x86_64-unknown-linux-gnu}
out=${3:-$(mktemp -d)}
repo=${DEVAGOTCHI_REPO:-EonsofStupid/devagotchi}
sha=$(gh api "repos/$repo/commits/$commit" --jq .sha)
run=$(gh api "repos/$repo/actions/workflows/release.yml/runs?branch=main&event=push&head_sha=$sha&status=success" \
    --jq '.workflow_runs[0].id // empty')
[ -n "$run" ] || { echo "fetch-main-build: no successful release build of $sha on main yet" >&2; exit 1; }
mkdir -p "$out"
gh run download "$run" -R "$repo" -n "artifacts-build-local-$target" -D "$out" >&2
found=0
for archive in "$out"/*.tar.xz "$out"/*.zip; do
    [ -e "$archive" ] || continue
    expected=$(cut -d' ' -f1 < "$archive.sha256" | tr 'A-F' 'a-f')
    actual=$(sha256sum "$archive" | cut -d' ' -f1)
    [ "$expected" = "$actual" ] || { echo "fetch-main-build: checksum mismatch for $archive" >&2; exit 1; }
    gh attestation verify "$archive" -R "$repo" >/dev/null \
        || { echo "fetch-main-build: no valid GitHub attestation for $archive" >&2; exit 1; }
    echo "fetch-main-build: $(basename "$archive") sha256 $actual, attested by $repo at ${sha:0:12}" >&2
    found=1
done
[ "$found" = 1 ] || { echo "fetch-main-build: the artifact held no archives" >&2; exit 1; }
echo "$out"
