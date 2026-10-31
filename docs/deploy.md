# Deploying the hub

This does not deploy anything by itself. It is the pipeline for a later run, aimed at the hosted hub.

## What may be deployed

Only a cargo-dist artifact built from a commit that is on `main`. `.github/workflows/release.yml` builds and attests every commit on `main` and publishes a GitHub Release only from a version tag. A pull-request build is not a release.

`deploy/linux/promote-hub.sh` refuses to continue unless:

- `DEVAGOTCHI_SHA` is a full commit that `git merge-base --is-ancestor` says is on `origin/main`
- `DEVAGOTCHI_STAGING_HOST` and `DEVAGOTCHI_PROD_HOST` are set
- `--confirm` is passed

It does not connect anywhere those variables are unset, and it contains no key. The Texas host and CT 110 are named in the script's comment so the operator knows which values to set. They are not defaults.

## Where it runs

Production is the hub already on Texas.

| | Production | Staging |
|---|---|---|
| Proxmox host | `tx-infra-prod` | the same host |
| Public address | `23.156.136.173` | the same address |
| Container | CT 110 | a second CT, not 110 |
| HTTPS name | `app.devagotchi.com` | `staging.app.devagotchi.com` |
| Edge | Cloudflare in front of the name | Cloudflare in front of the name |

Each instance has its own state directory, listen port, and hub process. The stock installer (`install-devagotchi.sh --role hub`) uses one account, `127.0.0.1:8787`, and `~/.local/state/devagotchi-hub`. Two hubs in CT 110 would share those. Staging is a second container on the same Proxmox host so the installer can run unchanged inside it. Caddy in that container proxies `staging.app.devagotchi.com` to its own loopback port. DNS for both names points at `23.156.136.173`; Cloudflare terminates the public TLS.

A second service inside CT 110 is not what the promote script installs. It would need another system account, another unit, another port, and another state directory, which this installer does not take.

## Order

1. Resolve the versioned archive for `DEVAGOTCHI_SHA` (the attested dist artifact for that commit, the same class of artifact `scripts/lay-down-dist.sh` already checks for machine builds).
2. Install that version on the staging container with `install-devagotchi.sh --role hub --version <version>`, beside the version already there. The installer keeps `/opt/devagotchi/versions/<version>` and points `current` at one.
3. Smoke staging. `GET /healthz` must be HTTP 200 with `auth.ready` true when the hub is in `devagotchi` mode. `GET /.well-known/oauth-protected-resource/mcp` must set `resource` to `{public_url}/mcp` and `authorization_servers` to the hub itself. The full connector path (dynamic registration, PKCE, upstream login, MCP `initialize`, `whoami`) is `tests/oauth_probe.py`, which CI runs against a fake provider. Do not run that probe against production or staging: it would create accounts.
4. If smoke fails, run the installer again with the previous version. That is the rollback (`deploy/linux/README.md`).
5. If smoke passes, install the same version on production (CT 110) and smoke it the same way. If production smoke fails, install the previous version again.

The script reaches the Proxmox host over SSH and, when a container id is set, runs the installer with `pct exec`. Nothing here SSHs until the variables below are set and `--confirm` is present. The addresses in the table are the operator's map. They are not defaults in the script.

```bash
DEVAGOTCHI_SHA=<commit on origin/main>
DEVAGOTCHI_VERSION=<release version>
DEVAGOTCHI_PREVIOUS_VERSION=<version to roll back to>
DEVAGOTCHI_STAGING_HOST=<user>@23.156.136.173
DEVAGOTCHI_STAGING_LXC=<staging CT id>
DEVAGOTCHI_STAGING_URL=https://staging.app.devagotchi.com
DEVAGOTCHI_PROD_HOST=<user>@23.156.136.173
DEVAGOTCHI_PROD_LXC=110
DEVAGOTCHI_PROD_URL=https://app.devagotchi.com
deploy/linux/promote-hub.sh --confirm
```

`DEVAGOTCHI_STAGING_HOST` and `DEVAGOTCHI_PROD_HOST` are `user@host` for SSH. They are usually the same Proxmox host. `DEVAGOTCHI_STAGING_LXC` and `DEVAGOTCHI_PROD_LXC` select the container. Leave an `LXC` variable empty only when that SSH target is already the container, not the Proxmox host.

`homelab-infra-dev` and `wi-infra-dev` are not in this repository. A new Devagotchi account does not receive any configured broker. Grants stay an operator edit, now keyed by the Devagotchi account id.

## What the script will not do

It will not create a Nomen user, print a key, or talk to `id.shippin.cloud`. Upstream discovery is the hub's own `/healthz` once the process is up.
