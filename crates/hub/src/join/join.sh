#!/bin/sh
# Devagotchi: join this Linux machine to @HUB@ as "@DEVICE@".
# Served by the hub for one join code; the code works once and expires 15 minutes after it
# was issued. Every download is checked against the checksum written here by the hub.
# Extra arguments go to install-devagotchi.sh: curl -fsSL <url> | sudo sh -s -- --workspace /srv/work
set -eu
hub='@HUB@'
device='@DEVICE@'
version='@VERSION@'
say() { printf 'devagotchi join: %s\n' "$*" >&2; }
[ "$(uname -s)" = Linux ] || { say "this one is for Linux; on Windows run in PowerShell: irm <the same URL>.ps1 | iex"; exit 1; }
[ "$(id -u)" = 0 ] || { say "run it as root: curl -fsSL <the same URL> | sudo sh"; exit 1; }
for tool in curl sha256sum tar xz bash systemctl; do
    command -v "$tool" >/dev/null 2>&1 || { say "needs $tool"; exit 1; }
done
case "$(uname -m)" in
    x86_64|amd64) target=x86_64-unknown-linux-gnu; sum='@SHA_LINUX_X86_64@' ;;
    aarch64|arm64) target=aarch64-unknown-linux-gnu; sum='@SHA_LINUX_AARCH64@' ;;
    *) say "no Devagotchi build for $(uname -m)"; exit 1 ;;
esac
[ -n "$sum" ] || { say "$hub has no $target build"; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM
get() {
    curl --proto '@PROTO@' -fsSL -o "$work/$1" "$hub/dist/$1" || { say "could not download $1 from $hub"; exit 1; }
    echo "$2  $work/$1" | sha256sum -c --status - || { say "$1 does not match the checksum $hub gave; nothing installed"; exit 1; }
}
archive=devagotchi-$target.tar.xz
get install-devagotchi.sh '@SHA_INSTALLER@'
get "$archive" "$sum"
echo "$sum  $archive" > "$work/$archive.sha256"
say "installing Devagotchi $version and joining $hub as $device"
printf '%s\n' '@CODE@' | bash "$work/install-devagotchi.sh" --version "$version" --artifact-dir "$work" \
    --hub "$hub" --device "$device" --enroll --exec-policy account "$@"
say "done: $device is on $hub. Tell your AI to use it by that name."
