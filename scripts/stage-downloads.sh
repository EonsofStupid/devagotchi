#!/usr/bin/env bash
# Lay down the builds a hub hands to joining machines (`/join`, `/dist`) from one commit's
# release artifacts, in the layout downloads.rs reads: the archive for every target, both
# installers, SHA256SUMS listing them all, and VERSION.
#
#   scripts/stage-downloads.sh ARTIFACTS_DIR VERSION OUT_DIR
#
# ARTIFACTS_DIR holds dist's archives and their .sha256 files, as the release workflow's
# `artifacts-build-local-*` artifacts unpack (image.yml, before the hub image is built); each
# archive is checked against its .sha256 and every target must be there, so an image never
# carries a partial set. The installers come from this checkout, the same commit. OUT_DIR is
# made, and must be new or empty.
set -euo pipefail
artifacts=${1:?usage: stage-downloads.sh ARTIFACTS_DIR VERSION OUT_DIR}
version=${2:?usage: stage-downloads.sh ARTIFACTS_DIR VERSION OUT_DIR}
out=${3:?usage: stage-downloads.sh ARTIFACTS_DIR VERSION OUT_DIR}
root=$(cd "$(dirname "$0")/.." && pwd)
die() { echo "stage-downloads: $*" >&2; exit 1; }

# The version goes into the join script as is: downloads.rs holds it to the same rule.
[[ $version =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$ ]] || die "VERSION $version is not a plain version"
[ -d "$artifacts" ] || die "$artifacts is not a directory"
mkdir -p "$out"
[ -z "$(ls -A "$out")" ] || die "$out is not empty"

for archive in devagotchi-x86_64-unknown-linux-gnu.tar.xz devagotchi-aarch64-unknown-linux-gnu.tar.xz \
    devagotchi-x86_64-pc-windows-msvc.zip devagotchi-aarch64-pc-windows-msvc.zip; do
    found=$(find "$artifacts" -type f -name "$archive" | head -n 1)
    [ -n "$found" ] || die "no $archive in $artifacts"
    [ -f "$found.sha256" ] || die "no $archive.sha256 beside $found"
    expected=$(cut -d' ' -f1 < "$found.sha256" | tr 'A-F' 'a-f')
    actual=$(sha256sum "$found" | cut -d' ' -f1)
    [ "$expected" = "$actual" ] || die "$archive has sha256 $actual, its .sha256 says $expected"
    cp "$found" "$out/$archive"
done
cp "$root/deploy/linux/install-devagotchi.sh" "$root/deploy/windows/install-devagotchi.ps1" "$out/"
printf '%s\n' "$version" > "$out/VERSION"
(cd "$out" && sha256sum -- devagotchi-* install-devagotchi.* > SHA256SUMS)
chmod 0755 "$out"
chmod 0644 "$out"/*
echo "stage-downloads: $(wc -l < "$out/SHA256SUMS") files for $version in $out" >&2
cat "$out/SHA256SUMS" >&2
