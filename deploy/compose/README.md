# A self-hosted Devagotchi hub with Docker Compose

The hub, its own Postgres, and Caddy for TLS, from one command on any Linux box with Docker:

```bash
git clone https://github.com/EonsofStupid/devagotchi && cd devagotchi/deploy/compose
cp .env.example .env        # set DEVAGOTCHI_DOMAIN to the name that points at this box
docker compose up -d
```

Then make the owner's passkey: run the command below on the box, and open the link it
prints (once, for 15 minutes) on the phone or computer that will hold the passkey.

```bash
docker compose exec hub devagotchi hub owner --state-dir /var/lib/devagotchi --config /etc/devagotchi/hub.json
```

AI clients connect to `https://<DEVAGOTCHI_DOMAIN>/mcp`: they register themselves, you approve
each one once with your passkey, and the hub issues and checks their tokens itself
(`crates/hub/README.md`, "AI clients sign in with the hub itself").

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

The defaults suit a 2 GB box. The hub's own settings (token lifetimes, limits) go in its config,
`configs.hub` in `compose.yaml` (`crates/hub/README.md`).

## Checking it

```bash
docker compose ps                                   # all three healthy or running
curl -fsS https://<DEVAGOTCHI_DOMAIN>/healthz       # the hub answers
docker compose logs hub | grep "ready"              # "devagotchi hub ready: … auth hub accounts"
```

CI brings this stack up on every change (`hub (compose)` in `.github/workflows/ci.yml`) and
checks the hub answers through Caddy and keeps its tables in this Postgres.
