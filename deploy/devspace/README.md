# Devagotchi devspace

One Linux tenant that holds everything about this project, survives restarts, and is
reachable from any device. It is also the first tenant of the hosted devspace the
plan describes: the same broker that runs on a laptop runs here headless, as a
`systemd --user` service, and is the engine the phone and remote MCP clients talk to.

| | |
|---|---|
| Base | Ubuntu 24.04 system container (Incus) or VM with systemd; kernel ≥ 5.15, cgroup v2 |
| Root | `/devspace` on its own volume: snapshot it, rebuild the container, re-attach, re-run `provision.sh` |
| User | `hades` (sudo, lingering, so jobs outlive logout) |
| Toolchains | rustup (compiler pinned by the repo's `rust-toolchain.toml`), Go (current stable), Python 3.12 + 3.13 via uv, Node 22 via fnm — all under `/devspace/toolchains`, nothing in `~` |
| Services | `devagotchi-broker` (user unit, starts once built), Desktop Commander remote agent (until Devagotchi replaces it), Claude Code |

## Layout

```text
/devspace/
├── repos/          git clones; the broker's workspace grant
│   └── devagotchi/
├── state/          XDG_STATE_HOME: devagotchi/ journal and logs
├── config/         XDG_CONFIG_HOME: devagotchi/broker.json
├── toolchains/     rustup cargo go gopath uv python python-bin uv-cache fnm npm
├── receipts/       verification and benchmark run directories (committed back under verification/ when they matter)
├── bin/            devspace-check; bridge-broker and bridge-mcp symlinks to the current build
└── env.sh          exports for shells (/etc/profile.d) and systemd user services (environment.d)
```

## Provision

```bash
# on the Incus host
incus profile create devspace
incus profile edit devspace < deploy/devspace/incus-profile.yaml
incus storage volume create default devspace-hades size=60GiB
incus launch images:ubuntu/24.04 devspace-hades -p default -p devspace
incus config device add devspace-hades devspace disk pool=default source=devspace-hades path=/devspace
incus file push deploy/devspace/provision.sh deploy/devspace/check.sh deploy/devspace/devagotchi-broker.service deploy/devspace/broker.devspace.json devspace-hades/root/
incus exec devspace-hades -- env DEVSPACE_USER=hades bash /root/provision.sh
incus exec devspace-hades -- su - hades -c devspace-check     # every line ok; paste it into the PR or receipt
```

`provision.sh` is idempotent; re-run it to move a toolchain version. Set `INSTALL_CARGO_TOOLS=1`
to also build `cargo-nextest` and `cargo-deny` (tooling PR of Increment 1). Any Ubuntu 24.04
machine works the same way without Incus: copy the four files and run the script as root.
It assumes direct egress, or a transparent proxy whose CA is in the system store (uv is told to
use that store, `UV_SYSTEM_CERTS=1`, because it otherwise trusts only its bundled roots); an
explicit `https_proxy` has to be set system-wide (`/etc/environment`) since `sudo` strips it.

## Build and start the broker

```bash
# inside the tenant, as hades
cd /devspace/repos && git clone https://github.com/EonsofStupid/devagotchi && cd devagotchi
cargo build --locked --release --bins
ln -sf /devspace/repos/devagotchi/target/release/bridge-broker /devspace/bin/bridge-broker
ln -sf /devspace/repos/devagotchi/target/release/bridge-mcp    /devspace/bin/bridge-mcp
systemctl --user start devagotchi-broker && systemctl --user status devagotchi-broker
journalctl --user -u devagotchi-broker -n 20     # "bridge-broker ready: /devspace/state/devagotchi/broker.sock"
python3 tests/devspace_broker_probe.py /devspace/state/devagotchi/broker.sock   # runs git --version under the broker, exit 0
```

Point any local MCP host at the adapter the usual way: `/devspace/bin/bridge-mcp --socket <path from the ready line>`.

Known limit today: the broker clears the environment for every job (S10 in the plan), so
`cargo`, `go` and `uv` fail under it until the environment-profile packet lands in G1
(they need `HOME`, `CARGO_HOME`, `GOPATH`). `git` and `rg` work now. Build in the tmux or
Claude Code session; use the broker for its lifecycle and for dogfooding the inspector.

## Connect remotely

| From | How |
|---|---|
| Laptop or phone shell | SSH over the Wardenclyffe WireGuard mesh, then `tmux attach -t dev` (a detached `tmux` session named `dev` is the working session; start it once) |
| Claude Code from phone or browser | inside tmux: `claude remote-control --name devspace` (server mode; Pro/Max/Team/Enterprise). Scan the QR code. Sessions resume for about four hours after the server stops: `claude remote-control --continue` |
| Claude.ai chat and Cowork (this kind of session) | the Desktop Commander remote agent: `npm` installed it; add the tenant as a device at <https://mcp.desktopcommander.app> and run the command the dashboard gives inside tmux. The tenant then appears next to Gus and dabo in the device list. This is the bridge Devagotchi replaces; keep it until the mesh packet (M1) ships |
| Claude Desktop, Cursor, Codex CLI on a laptop | until M1: SSH port-forward is not needed; run `bridge-mcp` on the laptop against a broker there. The remote path to this tenant's broker arrives with M1 (`bridge-mcp --remote <ticket>`) |

## Resume

Everything that matters is in three places and nowhere else: the repo on GitHub (code,
PRs, `docs/`), `/devspace` on the volume (journal, toolchains, receipts), and the plan doc.
A new session, human or agent, resumes by reading `CLAUDE.md`, `git log`, and
`systemctl --user status devagotchi-broker`. Snapshots of the `devspace-hades` volume are
the rollback; a restored snapshot brings running operations back as `unknown` with cause
`restart without kernel object`, which is the truthful state.
