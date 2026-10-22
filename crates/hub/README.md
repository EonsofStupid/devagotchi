# devagotchi-hub

The bastion (plan §11a, N13). Every AI reaches a machine through it: the hub proves who is
calling with the identity provider, checks that subject's grants, and routes the broker's
own four handle-based tools to the broker that owns the target machine. The hub never
admits a job; the broker does, and the job outlives the hub, the connection and the client.

```text
claude.ai / ChatGPT ──OAuth (Nomen)──┐
Claude Code / Codex ──device code────┤   ┌──────── devagotchi-hub ────────┐
your own agents ──service user───────┴──▶│ bearer → subject → grants → route │──▶ bridge-broker (gold-ws)
                                          │ audit: hash-chained SQLite        │──▶ bridge-broker (gus)      (M1)
                                          └───────────────────────────────────┘──▶ bridge-broker (dabo)     (M1)
```

## Run

```bash
devagotchi-hub --state-dir ~/.local/state/devagotchi-hub --config ~/.config/devagotchi-hub/hub.json
# devagotchi-hub ready: http://127.0.0.1:8787/mcp (1 brokers, auth oidc https://id.shippin.cloud)
```

```json
{
  "listen": "127.0.0.1:8787",
  "public_url": "https://hub.shippin.cloud",
  "auth": { "mode": "oidc", "issuer": "https://id.shippin.cloud", "audiences": ["<hub client or project id>"] },
  "brokers": [
    { "name": "gold-ws", "socket": "/home/dev/.local/state/devagotchi/broker.sock" },
    { "name": "gus", "link": true }
  ],
  "grants": [ { "subject": "<Nomen sub>", "brokers": ["gold-ws", "gus"] } ]
}
```

Unknown fields and unsafe values fail closed. The hub speaks plain HTTP; the edge (Caddy)
terminates TLS at `public_url`, which is also the RFC 9728 resource identifier. A broker is
either local (`socket`: the hub must run as the same OS user, since the IPC refuses
endpoints owned by anyone else) or linked (`"link": true`: a machine that dials out to `/link`).

## Machines on the link

A broker on another machine (gus, dabo, a server) is reached through
[`devagotchi-link`](../link/README.md), which holds one outbound WebSocket to `/link`. No port
opens on the machine. The device proves its enrolled Ed25519 key over a fresh challenge;
the hub then sends one call per routed request and waits up to 30 s for the answer.

```bash
devagotchi-hub enroll  --state-dir DIR --device gus   # one-time code, 15 minutes, stdout only; supersedes older ones
devagotchi-hub revoke  --state-dir DIR --device gus   # link closes within 2 s; pending codes cancelled
devagotchi-hub devices --state-dir DIR                # enrolled machines, JSON
```

A linked broker that is not connected answers `broker_offline`; one that never enrolled,
`not_enrolled`; a revoked one, `device_revoked`; one with 64 calls already waiting,
`link_busy`. A refused device is told only `refused`; why is in the audit. `/healthz` reports each linked broker as
`connected` without counting an asleep laptop against the hub's own health.

## Authentication

| Mode | Bearer token | Who it is for |
|---|---|---|
| `oidc` | A JWT from the issuer: `iss`, `exp`, `aud`, `sub` required; RS256/384/512 or ES256/384; keys from the issuer's JWKS, refreshed on an unknown `kid` at most once a minute | Nomen: device-code sessions (Claude Code, Codex, `shippin-bridge`), hosted clients registered through dynamic client registration, service users for your own agents |
| `dev` | A static token from a 0600 file `{ "<token>": "<subject>" }` | Local tests; a single machine without an identity provider |

Unauthenticated requests get `401` with `WWW-Authenticate: Bearer resource_metadata="<public_url>/.well-known/oauth-protected-resource"`;
the metadata names the issuer, which is how claude.ai and ChatGPT find Nomen. A bad token adds
`error="invalid_token"`. Nomen must issue JWT access tokens for the hub's audience (Zitadel:
the application's auth token type set to JWT); opaque tokens need the introspection path,
which is H2.

## Tools

`whoami` returns the proven subject and the brokers it may use. `operation_start`,
`operation_get`, `operation_read` and `operation_cancel` take `{ "broker": "<name>", "input": {…} }`
where `input` is exactly the broker's own contract. A subject without a grant for that
broker gets `not_granted`; an unknown name gets `unknown_broker`; the broker's own refusals
(`executable_not_allowed`, …) pass through unchanged.

## Audit and health

Every routed call appends one row to `audit.sqlite3` in the state directory: time, subject,
how it authenticated, tool, broker, operation id, outcome, `prev_hash`, `hash`. A call that
cannot be recorded is refused. `GET /healthz` reports each broker's reachability and walks
the chain (`{"ok": true, "brokers": [...], "audit": {"entries": N, "ok": true, "head": "…"}}`).

## Check

`python3 tests/hub_probe.py http://127.0.0.1:8787 --token <bearer> --broker gold-ws --cwd ~`
walks the surface end to end against a live broker and exits 0 only when every step holds.

## Not in H1

Trusting calls relayed by the Shippin front door (N19, D2), installer autostart for the
link, introspection for opaque tokens, a web UI, the agent-hub board (H3), harness
profiles (H4), the bundle (H5).
