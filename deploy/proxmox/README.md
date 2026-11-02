# The hub in a Proxmox VE container

`hub-up.sh` reloads one container on a Proxmox VE host as the Devagotchi hub, from the
image GitHub publishes, and leaves it deploying each new image by itself. As root on the
Proxmox host:

```bash
curl -fsSL https://raw.githubusercontent.com/EonsofStupid/devagotchi/main/deploy/proxmox/hub-up.sh \
  | bash -s -- --ct <id> --domain <name> [--rp-id <parent domain>]
```

| Step | What it does |
|---|---|
| Snapshot | `pct snapshot <id> devagotchi-<time>`; `pct rollback <id> devagotchi-<time> --start` undoes everything below. Storage without snapshots is reported, not refused |
| Docker | Turns on `nesting` (and `keyctl` for an unprivileged container) and restarts the container when they were off |
| Old hub | Stops and disables a `devagotchi-hub` unit installed there before (the `devagotchi` account's user unit, or a system unit); its files stay. Refuses to go on if anything else still holds the hub's port |
| The stack | Copies `deploy/compose/` from one commit of the repository in and runs `install.sh` there: Docker from Docker's repository, the hub image pinned by digest, its own Postgres, the 5-minute update timer (deploys each new image of the track, keeps it only if healthy, rolls back otherwise and never retries that digest; backs the database up first) and the nightly backup |
| The edge | If the container's own Caddy already serves the name, the hub takes `127.0.0.1:<port>` behind it and that Caddy keeps its certificates and other sites; otherwise the stack's Caddy takes ports 80 and 443 |
| Check | `https://<name>/healthz` must answer from the new hub (its audit head), else it stops and says so |
| Claim | Prints the claim link from the hub's log: open it on the phone that will hold the owner's passkey |

Re-running it is safe. Every value comes from its flags (`--help`); none names a host.
`tests/proxmox_hub_up.py` runs it against a stand-in Proxmox host in CI, and
`tests/deploy_scripts.sh` runs what it installs for real.
