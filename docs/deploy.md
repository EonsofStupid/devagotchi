# Deploying the hub

This does not deploy anything by itself. It is the pipeline for a later run, aimed at the hosted hub.

## What may be deployed

Only a cargo-dist artifact built from a commit that is on `main`. `.github/workflows/release.yml` builds and attests every commit on `main` and publishes a GitHub Release only from a version tag. A pull-request build is not a release.

`deploy/linux/promote-hub.sh` refuses to continue unless:

- `DEVAGOTCHI_SHA` is a full commit that `git merge-base --is-ancestor` says is on `origin/main`
- `DEVAGOTCHI_STAGING_HOST` and `DEVAGOTCHI_PROD_HOST` are set
- `--confirm` is passed

It does not contain a host, a key, or a container id. Those stay in the environment of the person who runs it.

## Order

1. Resolve the versioned archive for `DEVAGOTCHI_SHA` (the attested dist artifact for that commit, the same class of artifact `scripts/lay-down-dist.sh` already checks for machine builds).
2. Install that version on the staging host with `install-devagotchi.sh --role hub --version <version>`, beside the version already there. The installer keeps `/opt/devagotchi/versions/<version>` and points `current` at one.
3. Smoke staging. `GET /healthz` must be HTTP 200 with `auth.ready` true when the hub is in `devagotchi` mode. `GET /.well-known/oauth-protected-resource/mcp` must set `resource` to `{public_url}/mcp` and `authorization_servers` to the hub itself. The full connector path (dynamic registration, PKCE, upstream login, MCP `initialize`, `whoami`) is `tests/oauth_probe.py`, which CI runs against a fake provider. Do not run that probe against production: it would create accounts.
4. If smoke fails, run the installer again with the previous version. That is the rollback (`deploy/linux/README.md`).
5. If smoke passes, install the same version on production and smoke it the same way. If production smoke fails, install the previous version again.

Production in this document is the hosted hub on Texas (Proxmox LXC 110, the machine the broker name `tx-infra-prod` is the test fixture for — the container id is not in the repo). `DEVAGOTCHI_PROD_HOST` is how the script reaches it (`user@host`, and `DEVAGOTCHI_PROD_LXC` if the command must go through `pct exec`). Nothing here SSHs until those variables are set and `--confirm` is present.

`homelab-infra-dev` and `wi-infra-dev` are not in this repository. A new Devagotchi account does not receive any configured broker. Grants stay an operator edit, now keyed by the Devagotchi account id.

## What the script will not do

It will not create a Nomen user, print a key, or talk to `id.shippin.cloud`. Upstream discovery is the hub's own `/healthz` once the process is up.
