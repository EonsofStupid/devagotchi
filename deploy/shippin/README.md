# Devagotchi on Shippin.Cloud (Gold)

Shippin.Cloud on Gold is the first hosted deployment of Devagotchi; devagotchi.net is the
second (plan §12, N11). On Gold, Devagotchi is the execution engine inside every tenant
workspace: one broker per workspace, owned by the workspace user, supervised by that
workspace's `systemd --user`, journal and config under the user's XDG directories so they
survive stop/start and ride along in home snapshots. The control plane (console + hostd)
owns the workspace lifecycle; Devagotchi owns the jobs inside it. Nothing here runs on the
Gold host or in the `platform` project.

| Layer | Owner | Devagotchi's part |
|---|---|---|
| Tenant → boundary VM → project → workspace | console + hostd (Shippin control plane) | none; it is placed, not placing |
| Inside a workspace (`w-<uuid>`, user `dev`) | hostd image template (`shippin-ws-init`, bridge CA) | `bridge-broker` as a user service, `bridge-mcp` on `$PATH` |
| Reaching it from a laptop agent (Claude Code, Codex CLI, Gemini CLI, Cursor) | `shippin-bridge login` → `shippin-bridge mcp` / `ssh` (N4 bridge) | `bridge-mcp --socket <state>/broker.sock` is what the bridge's MCP mode should exec inside the workspace |
| Reaching it from a hosted frontier client (claude.ai, ChatGPT) | console `/mcp` lane with Nomen OAuth (DCR now, CIMD later) | the same handle-based tools; the lane proxies to the workspace broker through hostd (A1 contract) |

## Install inside a workspace

```bash
# as the workspace user, in any Debian 13 / Ubuntu workspace with egress to GitHub
curl -fsSL https://raw.githubusercontent.com/EonsofStupid/devagotchi/main/deploy/shippin/install-workspace.sh | bash
# pinned, or offline from pre-fetched release tarballs (image bake):
DEVAGOTCHI_VERSION=0.1.0 bash install-workspace.sh
DEVAGOTCHI_ARTIFACT_DIR=/srv/devagotchi-release bash install-workspace.sh
```

The installer downloads the two release tarballs (`devagotchi-broker-*`, `devagotchi-mcp-*`,
built and attested by `.github/workflows/release.yml`), checks their sha256 and, when `gh` is
present, their provenance, installs them under `~/.local/share/devagotchi/<version>/bin` with
`~/.local/bin` symlinks, writes `~/.config/devagotchi/broker.json` with grants for the toolchains
actually present (git, rg, node, npm, python3, cargo, rustup, go, uv), and enables a
`devagotchi-broker` user unit (`Restart=always`, `Delegate=yes`). Without `systemd --user` it starts
the broker unsupervised and says so. Re-running moves the version; the config is kept.

Check: `systemctl --user status devagotchi-broker` shows `bridge-broker ready: ~/.local/state/devagotchi/broker.sock`;
`python3 tests/devspace_broker_probe.py ~/.local/state/devagotchi/broker.sock --adapter ~/.local/bin/bridge-mcp --cwd ~`
runs `git --version` under the broker and confirms an ungranted executable is refused.

## Baking it into the workspace image

The `:ai1` images (`shippin/dev-debian13-ai1` and the `tpl-*-ai1` templates) are built by
shippin-console's `deploy/image/build-image.sh` + `deploy/image/ai/install.sh`, which already pin
Node and the AI CLIs by checksum. The next image (`:ai2`) adds one step: run this installer as the
template user with `DEVAGOTCHI_VERSION` pinned and `DEVAGOTCHI_ARTIFACT_DIR` pointing at tarballs
fetched and verified at build time, so workspaces never reach GitHub at first boot. Linger for the
workspace user is hostd's job (it already runs as root in `shippin-ws-init`); until it is set, jobs
end with the last session, which the installer reports.

## What the control plane needs from Devagotchi, and when

| Need | Where it lands |
|---|---|
| Health through the execution path (hostd `ai-status`-style probe) | the broker's health probe (plan §3) — `bridge-mcp` `operation_start` of a granted no-op |
| Environment profiles so `cargo`, `go`, `uv` run under the broker | G1 environment-profile packet (S10 today clears the job environment) |
| Per-agent identity in the journal (which frontier client ran what) | task-graph subjects (N10); until then the bridge session id in the audit chain |
| Remote lane for hosted clients | console `/mcp` (shippin-console) with the A1 contract; Devagotchi's runtime API takes it over at M1 — parked as N12 |

## Not decided here

- Broker per workspace (this document) versus one broker per boundary VM: per workspace keeps
  the per-user ownership model (plan D02) and the journal in the user's home; per VM would need
  multi-user journals and is not planned.
- Whether `shippin-bridge mcp` execs `bridge-mcp` inside the workspace or speaks the broker's IPC
  directly: exec is one line in hostd's session commands and keeps one adapter binary; direct IPC
  waits for the stable mesh contract (M1).
