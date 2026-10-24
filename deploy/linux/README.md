# Devagotchi on a Linux server

`install-devagotchi.sh` puts the broker and the link on a Linux server so AI reaches it through
a Devagotchi hub, the same way on Gold, Platinum, Wisconsin and Texas (plan §11b, step 1), and
with `--role hub` puts the hub itself on a server (below). It
needs systemd as init; it is tested on Ubuntu 24.04 in CI, on x86_64 and on 64-bit ARM
(aarch64), and targets Debian 12 and 13 too. It picks the release build from the machine
(`--print-target` shows which); release binaries need glibc 2.35 or newer.

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

## The hub role: devagotchi.com, or any hub host

The same installer, `--role hub`, puts the hub on a server: the bastion agents reach at
`<public_url>/mcp`, machines dial into at `/link`, and the project boards live on
(`docs/agent-board.md`). The hub listens on loopback; Caddy terminates TLS in front of it and
fetches the certificate itself once DNS points at the host.

```bash
sudo ./install-devagotchi.sh --role hub --version 0.2.0 --public-url https://devagotchi.com \
     --board devagotchi --person jesse --agent claude-on-gus --agent codex-on-dabo --caddy
sudo ./install-devagotchi.sh --role hub --agent gemini-on-wi1     # one more token, printed once
sudo ./install-devagotchi.sh --role hub --version 0.2.1            # upgrade; config, tokens, boards kept
```

| | |
|---|---|
| Service | `devagotchi-hub` as the account's `systemd --user` unit; `Restart=always`, lingering |
| Config | `~/.config/devagotchi/hub.json` (0600): `listen`, `public_url`, dev-token sign-in, `brokers`, `boards`, `grants`. Written once; afterwards yours — a re-run prints the lines to add, and the hub restarts only when something it runs changed |
| Tokens | `~/.config/devagotchi/hub-tokens.json` (0600): `--person NAME` and `--agent NAME` mint a bearer token each, shown once and never logged; the hub reads the file on every request, so a new token needs no restart. A person may force-release and move tickets by hand; an agent never |
| Boards | `--board NAME` initialises `~/projects/NAME` (or `--board NAME=/abs/dir`, which the account must be able to write) with `devagotchi-hub board init --agents-md`, lists it in the config, and grants it to every `--person` / `--agent` of the same run |
| Edge | `--caddy` installs Caddy from the distribution when missing, writes `/etc/caddy/devagotchi-hub.caddy` (`<host> { reverse_proxy 127.0.0.1:8787 }`), imports it from the Caddyfile, validates and reloads. An `http://host:port` public URL serves plain HTTP (tests, loopback) |
| Machines | add `{ "name": "<device>", "link": true }` to `brokers` and a grant, restart the hub, `devagotchi-hub enroll --state-dir ~/.local/state/devagotchi-hub --device <device>` as the account, and run the machine role there with `--enroll` |

An agent's MCP config then points at `https://devagotchi.com/mcp` with its token as the bearer
token; `whoami` shows what it may reach, `board_prime` is where a session starts. Shippin
sign-in (`auth.mode` nomen/oidc, `web`) replaces dev tokens by editing the config; the installer
does not write those sections.

## What it does not do

It grants no root. Root changes on a server go through gold-policy (plan §11b, step 6), and the
break-glass path is separate (step 7). If the account is in an admin group the installer says
so, because anything an AI runs through the broker can then use it.

## Check

`deploy/linux/ci-install-check.sh` runs in CI on a real systemd host, x86_64 and aarch64
(`host-install` jobs): the release build it picks matches the runner, install, a real operation
through the broker, recovery after `kill -9`, joining a hub with a piped code and a real
operation through the hub and the link, an upgrade that keeps the config and the enrollment,
recovery of the link, a revoke that leaves the link down with exit 2, the hub role on the same
host behind Caddy (a board initialised, tokens minted, a ticket created by the person and claimed
and completed by the agent through Caddy, a re-run that only mints a token), and uninstall.
