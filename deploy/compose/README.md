# A self-hosted Devagotchi hub with Docker Compose

The hub, its own Postgres, and Caddy for TLS, from one command on any Linux box with Docker:

```bash
git clone https://github.com/EonsofStupid/devagotchi && cd devagotchi/deploy/compose
cp .env.example .env        # set DEVAGOTCHI_DOMAIN to the name that points at this box
docker compose up -d
```

Then claim it: the hub prints a claim link to its log when it starts. Open that link on the
phone or computer that will hold your passkey, give your name, and press **Claim this hub
and make my passkey**. You are signed in as its owner.

```bash
docker compose logs hub | grep -A1 "Claim it in a browser"
#   https://<DEVAGOTCHI_DOMAIN>/claim#XXXX-XXXX-XXXX-XXXX-XXXX-XXXX
```

Until then, `https://<DEVAGOTCHI_DOMAIN>/` offers only the claim, and only the code from the
log claims it: reading the log takes this box or its container platform, so nobody who just
found the address can. The code works for a day (`claim_secs`); `docker compose restart hub`
prints a new one, and only the newest works. Your first passkey ends it for good. On the box,
`docker compose exec hub devagotchi hub owner --state-dir /var/lib/devagotchi --config
/etc/devagotchi/hub.json` prints a one-time owner link instead, and later adds passkeys
(`crates/hub/README.md`).

AI clients connect to `https://<DEVAGOTCHI_DOMAIN>/mcp`: they register themselves, you approve
each one once with your passkey, and the hub issues and checks their tokens itself
(`crates/hub/README.md`, "AI clients sign in with the hub itself"). From a terminal,
`devagotchi login --hub https://<DEVAGOTCHI_DOMAIN>` shows a link and a code; approve it on
the hub's `/device` page, signed in with your passkey, and the CLI (and `devagotchi mcp --hub`)
works as your account.

## On a server, kept current by itself

`install.sh` does the steps above on a Debian or Ubuntu server, and more: Docker from
Docker's repository if it is missing, `.env` from its flags (Postgres sized for the box), the
image pinned by digest, and two systemd timers:

- `devagotchi-hub-update.timer`, every 5 minutes, runs `update.sh`. When the track
  (`DEVAGOTCHI_TRACK`, `:main` by default) points at a new digest, it backs the database up,
  deploys that digest and keeps it only if `/healthz` answers healthy. Otherwise it goes back
  to the digest that ran before, which migrations allow (they add; an older hub opens a
  newer store), and never tries that digest again. What it did is in
  `journalctl -u devagotchi-hub-update`. The one exception is the tenant migration (0007):
  going back across it takes the backup made before that deploy (below).
- The deploy files come from GitHub with the image. Each image GitHub builds is labelled
  with its commit (`org.opencontainers.image.revision`); `update.sh` fetches `compose.yaml`,
  `update.sh` and `backup.sh` of that same commit from
  `raw.githubusercontent.com/<DEVAGOTCHI_REPO>/<commit>/deploy/compose/`, checks the new
  `compose.yaml` reads with this `.env`, and swaps them in with the image. If the hub is not
  healthy, the files that ran before go back with the image that ran before. When the
  commit's files cannot be fetched, the image is not deployed. `.env` records the commit in
  `DEVAGOTCHI_REVISION`. An image with no commit label (a local build) keeps the files here.
- `devagotchi-hub-backup.timer`, nightly, runs `backup.sh`: a `pg_dump` of the database and
  an archive of the state volume in `/var/backups/devagotchi-hub`, 14 days kept. Copy them
  off the host too.

```bash
sudo ./install.sh --domain hub.example.com                   # with this stack's Caddy
sudo ./install.sh --domain hub.example.com --edge external   # behind a proxy you run; it
                                                             # reaches the hub on --hub-bind:--hub-port
```

In a Proxmox VE container, `deploy/proxmox/hub-up.sh` runs it for you (`deploy/proxmox/README.md`).

### The hub's site

What the hub's pages say and look like (its name, copy, colours, pictures, the sign-in email
and the name AI clients see) is its site, a directory (`crates/hub/README.md`, "The site"):

```bash
sudo ./install.sh --domain hub.example.com --site /srv/hub-site    # install with it
sudo ./install.sh --site /srv/hub-site --site-only                 # later: the site alone
sudo ./install.sh --domain hub.example.com --no-site               # back to the built-in one
```

`--site DIR` copies DIR beside the live site, readable by the hub's unprivileged user, and
checks it with the hub image itself (`devagotchi hub check-site`). One that does not check
out is refused with what is wrong and where, and never reaches the hub. One that does goes
into `site/` under `--dir` (changed in place, so the running container sees it), and `.env`
names it in `DEVAGOTCHI_SITE_DIR`. A re-run without `--site` keeps it. `--site-only` puts a
site in place on a hub already installed and touches nothing else; `--no-site` goes back to
the site built into the image.

The hub follows the directory: edit a file in `site/` (or put a new site in with
`--site-only`) and it is served within a few seconds once it checks out, with no rebuild and
no restart. `docker compose up -d hub` applies it too, and picks up a changed
`DEVAGOTCHI_SITE_DIR`. A change that does not check out is refused: the hub keeps serving
the site it had and says why in `docker compose logs hub`.

### Rolling back across the tenant migration (0007)

Migration 0007 (increment 60) moves the hub's tables into the `devagotchi` schema, renames
them and puts each tenant's rows behind row-level security (`crates/hub/README.md`, "The
store"). It is the one migration that does not only add: a hub image from before it cannot
read the store after it. If the first image with 0007 is not healthy, `update.sh` puts the
image before it back, which then cannot start either. The journal says "nothing healthy to
roll back to". Restore the database from the backup `update.sh` took just before that
deploy, with the hub stopped:

```bash
cd /opt/devagotchi-hub
sudo systemctl stop devagotchi-hub-update.timer
docker compose stop hub
dump=$(ls -t /var/lib/devagotchi-hub-update/pre-deploy/db-*.dump | head -1)   # just before the deploy
docker compose exec -T postgres psql -h /var/run/postgresql -U devagotchi -d devagotchi \
  -c 'DROP SCHEMA devagotchi CASCADE'
docker compose exec -T postgres pg_restore -h /var/run/postgresql -U devagotchi -d devagotchi \
  --clean --if-exists < "$dump"
docker compose up -d hub              # .env already names the image before (update.sh set it)
curl -fsS https://<DEVAGOTCHI_DOMAIN>/healthz
sudo systemctl start devagotchi-hub-update.timer
```

Dropping `devagotchi` first matters: `pg_restore --clean` removes only what the backup holds
(the tables before 0007, in `public`). The digest that failed stays in
`/var/lib/devagotchi-hub-update/bad` and is not tried again. The next image runs 0007 afresh
on the restored store. Anything written between the backup and the restore is lost: on a
deploy that never became healthy, that is at most the minutes it ran.

### A hosted hub, its operators and its sign-ins

```bash
sudo ./install.sh --domain app.example.com --rp-id example.com --tenancy multi \
  --operators email:you@example.com \
  --google --github --nomen https://id.shippin.cloud \
  --email-from "Devagotchi <sign-in@example.com>" \
  --env-file /root/hub-secrets.env
```

- `--tenancy multi` makes it a hosted hub: everyone who connects gets an account and a tenant
  of their own, and it is never claimed (`crates/hub/README.md`, "A hosted hub").
- `--operators` lists who operates it: `email:<verified address>`, or `<provider>:<subject>`
  (`google:`, `github:`, `nomen:`). Their sign-in is the hub's owner, with no claim. A
  one-owner hub with operators set is not claimed either.
- `--google`, `--github`, `--nomen ISSUER` (Sign in with Shippin) and `--email-from ADDRESS`
  (sign-in links by email) turn on each way in. Their ids and secrets come from `--env-file`
  (KEY=VALUE lines, as the secret store writes them) or from the environment, and stay in
  `.env` (0600): `DEVAGOTCHI_GOOGLE_CLIENT_ID`/`_SECRET`, `DEVAGOTCHI_GITHUB_CLIENT_ID`/`_SECRET`,
  `DEVAGOTCHI_NOMEN_CLIENT_ID` (and `_SECRET` if its application has one),
  `DEVAGOTCHI_SMTP_URL` (`smtps://user@host:465` or `smtp://user@host:587`, never a password)
  and `DEVAGOTCHI_SMTP_PASSWORD`. A re-run keeps the values `.env` already has for any it is
  not given. Each provider's redirect URI is `https://<domain>/auth/provider/<google|github|nomen>/callback`.
- `--trusted-proxies` lists the proxies whose `X-Forwarded-For` names the client (for the
  per-address limit on new accounts); by default private and loopback addresses, which is
  what Caddy on the compose network and an edge proxy on the host are.

## The image

GitHub builds the hub image from every commit that lands on `main` (and every release tag),
natively on x86_64 and ARM64, and publishes one multi-arch image to GHCR
(`.github/workflows/image.yml`): `:main` and `:sha-<commit>` for main, the version and
`:latest` for a release. Each carries a signed build-provenance attestation, so you can check
the image you pulled was built by this repository's workflow from a given commit:

```bash
gh attestation verify oci://ghcr.io/eonsofstupid/devagotchi:main --owner EonsofStupid
```

Pin a commit with `DEVAGOTCHI_IMAGE=ghcr.io/eonsofstupid/devagotchi:sha-<commit>`, or build
from this checkout with `docker compose up --build`.

## What runs

| Service | What it is | Reached from |
|---|---|---|
| `postgres` | Postgres 18, the hub's store: accounts, passkeys, sessions, AI connections | Nothing over the network: it has no network (`network_mode: none`) and listens on a Unix socket in a volume only the hub mounts. With no TCP port there is no password to keep. Data checksums are on. |
| `hub` | The one `devagotchi` binary (D20), built by GitHub from a known commit; no shell, runs as `nonroot` with no capabilities and no privilege escalation, writing only to its state volume | Caddy, on the compose network |
| `caddy` | TLS for `DEVAGOTCHI_DOMAIN`, certificates from Let's Encrypt | Ports 80 and 443 (and 443/udp for HTTP/3) |

State lives in named volumes: `postgres-data` (the database), `hub-state` (the hub's keys and
machine enrolments), `caddy-data` (certificates). Back up `postgres-data` and `hub-state`.

## Settings

Everything is in `.env`; each has a default (`.env.example` lists them).

| Variable | Default | |
|---|---|---|
| `DEVAGOTCHI_DOMAIN` | (required) | The name the hub is reached at; Caddy gets its certificate for it. `localhost` works for a try-out, with Caddy's own certificate. |
| `DEVAGOTCHI_PUBLIC_URL` | `https://<DEVAGOTCHI_DOMAIN>` | The URL clients use, when it differs (another port, another proxy in front). Passkeys and AI clients are bound to it. |
| `DEVAGOTCHI_IMAGE` | `ghcr.io/eonsofstupid/devagotchi:main` | The hub image. `docker compose up --build` builds it from this checkout instead. |
| `DEVAGOTCHI_HTTP_PORT`, `DEVAGOTCHI_HTTPS_PORT` | `80`, `443` | Where Caddy listens on the host. |
| `DEVAGOTCHI_PG_SHARED_BUFFERS` | `256MB` | About a quarter of the box's RAM. |
| `DEVAGOTCHI_PG_EFFECTIVE_CACHE_SIZE` | `768MB` | About three quarters of the box's RAM. |
| `DEVAGOTCHI_PG_MAX_CONNECTIONS` | `50` | Each hub instance holds up to 16. |
| `DEVAGOTCHI_PG_SLOW_MS` | `500` | Statements slower than this are logged. |
| `DEVAGOTCHI_HUB_TENANCY` | `single` | `multi` for a hosted hub. |
| `DEVAGOTCHI_HUB_OPERATORS` | (none) | Who operates the hub, comma separated. |
| `DEVAGOTCHI_HUB_PROVIDERS` | `[]` | The sign-in providers, JSON as `auth.providers` takes it; `install.sh` writes it from `--google`, `--github`, `--nomen`. |
| `DEVAGOTCHI_HUB_EMAIL` | `null` | The email link's mail server, JSON as `auth.email` takes it; `install.sh` writes it from `--email-from`. |
| `DEVAGOTCHI_HUB_TRUSTED_PROXIES` | private and loopback | JSON list of addresses or CIDR ranges. |
| `DEVAGOTCHI_GOOGLE_CLIENT_ID` … `DEVAGOTCHI_SMTP_PASSWORD` | (none) | The providers' ids and secrets, from the secret store. |
| `DEVAGOTCHI_SITE_DIR` | (none: the built-in site) | The hub's site directory on this host, mounted read-only; `install.sh --site` sets it. |

The defaults suit a 2 GB box. The hub's own settings (token lifetimes, the claim code's, limits) go in its config,
`configs.hub` in `compose.yaml` (`crates/hub/README.md`).

The hub's site is the directory `DEVAGOTCHI_SITE_DIR` names, mounted read-only; with it
unset, the empty `site/` beside `compose.yaml` stands in and the hub serves the site built
into the image ("The hub's site", above).

## Checking it

```bash
docker compose ps                                   # all three healthy or running
curl -fsS https://<DEVAGOTCHI_DOMAIN>/healthz       # the hub answers
docker compose logs hub | grep "ready"              # "devagotchi hub ready: … auth hub accounts"
curl -fsS https://<DEVAGOTCHI_DOMAIN>/ | grep -o "Claim this hub\|Get started\|Sign in" | head -1
```

CI brings this stack up on every change (`hub (compose)` in `.github/workflows/ci.yml`) and
checks the hub answers through Caddy, keeps its tables in this Postgres, and is claimed with
the code from its log; `e2e` claims a fresh hub in Chromium with a virtual passkey
(`tests/claim_browser.py`).
