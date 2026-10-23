# Devagotchi on a Linux server

`install-devagotchi.sh` puts the broker and the link on a Linux server so AI reaches it through
a Devagotchi hub, the same way on Gold, Platinum, Wisconsin and Texas (plan §11b, step 1). It
needs systemd as init; it is tested on Ubuntu 24.04 in CI and targets Debian 12 and 13 too.

| | |
|---|---|
| Account | a dedicated system account (default `devagotchi`, home `/var/lib/devagotchi`), no password, never root |
| Services | `devagotchi-broker` and `devagotchi-link` as that account's `systemd --user` units; lingering starts them at boot with nobody logged in |
| Binaries | `/opt/devagotchi/versions/<version>/`, root-owned 0755; `/opt/devagotchi/current` points at one. What runs as the account cannot replace them |
| Config | `~/.config/devagotchi/broker.json` (0600): the workspace and the executables the broker may start, detected once (`git`, `rg`, `python3`, `node`, `cargo`, `rustup`, `go`, `uv`) plus `--grant` paths; kept on every re-run |
| Network | nothing listens; the link dials out to the hub and reconnects by itself |

## Install, join a hub, upgrade, remove

```bash
sudo ./install-devagotchi.sh --version 0.2.0 --grant /usr/bin/journalctl   # release archives, sha256 checked
# on the hub host: devagotchi-hub enroll --state-dir … --device wi1   (prints a one-time code)
sudo ./install-devagotchi.sh --hub https://hub.example.com --device wi1 --enroll   # paste the code
sudo ./install-devagotchi.sh --version 0.2.1          # upgrade; --version 0.2.0 again rolls back
sudo ./install-devagotchi.sh --uninstall              # stops and removes the services; files stay
```

The code is read from stdin (a hidden prompt on a terminal) before anything else runs, and
reaches the link on its stdin: never an argument other users could list, a file or a log line.
`--bin-dir target/release` installs a local build instead of a release.

A re-run restarts a service only when what it runs changed (a new version, its unit, the config,
a new enrollment): on the subtree and process-group supervision rungs a broker restart ends its
jobs. The link restarts after a crash; when the hub refuses the machine (revoked, unknown) the
link exits 2 and systemd leaves it down (`RestartPreventExitStatus=2`) instead of retrying.

Status and logs, as root:

```bash
sudo -u devagotchi XDG_RUNTIME_DIR=/run/user/$(id -u devagotchi) systemctl --user status devagotchi-broker devagotchi-link
journalctl _SYSTEMD_USER_UNIT=devagotchi-link.service -n 50
```

## What it does not do

It grants no root. Root changes on a server go through gold-policy (plan §11b, step 6), and the
break-glass path is separate (step 7). If the account is in an admin group the installer says
so, because anything an AI runs through the broker can then use it.

## Check

`deploy/linux/ci-install-check.sh` runs in CI on a real systemd host (`host-install` job):
install, a real operation through the broker, recovery after `kill -9`, joining a hub with a
piped code and a real operation through the hub and the link, an upgrade that keeps the config
and the enrollment, recovery of the link, a revoke that leaves the link down with exit 2, and
uninstall.
