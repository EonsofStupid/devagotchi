# devagotchi-hub

The bastion (plan §11a, N13). Every AI reaches a machine through it: the hub proves who is
calling with the identity provider, checks that subject's grants, and routes the broker's
own four handle-based tools to the broker that owns the target machine. The hub never
admits a job; the broker does, and the job outlives the hub, the connection and the client.

```text
claude.ai / ChatGPT ──mcp.shippin.cloud──┐  (your own Nomen token, passed through)
Claude Code / Codex ──device code────────┤   ┌──────── devagotchi-hub ────────┐
your own agents ──service user───────────┴──▶│ bearer → subject → grants → route │──▶ bridge-broker (gold-ws)
                                          │ audit: hash-chained SQLite        │──▶ bridge-broker (gus)      (M1)
                                          └───────────────────────────────────┘──▶ bridge-broker (dabo)     (M1)
```

## Run

```bash
devagotchi-hub --state-dir ~/.local/state/devagotchi-hub --config ~/.config/devagotchi-hub/hub.json
# devagotchi-hub ready: http://127.0.0.1:8787/mcp (2 brokers, auth nomen https://id.shippin.cloud)
# devagotchi-hub owner: 300000000000000001 reaches every broker
```

```json
{
  "listen": "127.0.0.1:8787",
  "public_url": "https://hub.shippin.cloud",
  "auth": {
    "mode": "nomen",
    "issuer": "https://id.shippin.cloud",
    "audiences": ["<Nomen project id the hub's API application belongs to>"],
    "key_file": "/etc/devagotchi-hub/nomen-key.json"
  },
  "owner": "<your Nomen account ID (sub)>",
  "brokers": [
    { "name": "gold-ws", "socket": "/home/dev/.local/state/devagotchi/broker.sock" },
    { "name": "gus", "link": true }
  ],
  "grants": [ { "subject": "<a teammate's Nomen sub>", "brokers": ["gold-ws"] } ]
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
| `nomen` | Any Nomen access token, opaque or JWT, checked by asking Nomen (introspection) as the hub's own API application | Shippin: your sign-in at console.shippin.cloud, relayed by mcp.shippin.cloud; `shippin-bridge` sessions |
| `oidc` | A JWT from any OpenID Connect issuer: `iss`, `exp`, `aud`, `sub` required; RS256/384/512 or ES256/384; keys from the issuer's JWKS, refreshed on an unknown `kid` at most once a minute | Self-hosters whose identity provider issues JWT access tokens |
| `dev` | A static token from a 0600 file `{ "<token>": "<subject>" }` | Local tests; a single machine without an identity provider |

Unauthenticated requests get `401` with `WWW-Authenticate: Bearer resource_metadata="<public_url>/.well-known/oauth-protected-resource"`;
the metadata names the issuer. A bad token adds `error="invalid_token"`. When the identity
provider cannot be reached the hub answers `503`, never `401`.

## Shippin sign-in (Nomen)

Nomen's access tokens are opaque: only Nomen can say whose they are. The hub asks it about
each token (RFC 7662 introspection), signing in as its own API application with a
private-key JWT. Nomen answers `active` only when the token is live and the hub's
application is in its audience; the hub then requires the issuer, an accepted project in
`aud`, an unexpired `exp` and an account ID (`sub`) itself.

Set up once in Nomen (a Gold change, through gold-policy):

1. In the project the hub belongs to, add an **API** application with **Private Key JWT**.
2. Add a key and download it. That JSON (`type`, `keyId`, `key`, `clientId`) is `auth.key_file`;
   keep it in Vaultix and write it at deploy as the hub's user, mode 0600. The hub refuses
   a file others can read, and its errors never print the key.
3. Put that project's id in `auth.audiences`. Clients whose tokens should reach the hub
   (mcp.shippin.cloud's app) must be in the same project, or request the scope
   `urn:zitadel:iam:org:project:id:<project id>:aud`.
4. Set `owner` to your account ID. Sign in and call `whoami` once if you are not sure which it is.

What running Shippin taught, and what the hub does about it:

| What happened running Shippin | What the hub does |
|---|---|
| Two active accounts with the same display name stopped a deploy until one was chosen | Grants and `owner` take the account ID (`sub`) only; an email or a name with spaces is refused at start. `whoami` shows the ID, the login name and whether you are the owner |
| A devspace created by a machine user locked its owner out, because only the creator could connect | Access comes from `owner` and grants, never from who created something |
| `tenant_owners` drifted from Nomen and was silently re-created | The hub keeps no copy of who owns what; it asks Nomen on every new token |
| A failed membership lookup in the console quietly meant "no roles" | Nomen down, slow, or refusing the hub's own key is `503 issuer_unavailable`, never a refusal, and is never remembered |
| One way in (SSH keys) became a lockout | `owner` reaches every broker and can only be changed by editing this file. The hub starts without Nomen, and `enroll`, `revoke` and `devices` work on the host with Nomen down |
| Nobody could test as the owner without borrowing a session | `tests/nomen_probe.py` runs the real binary against a fake Nomen; a live check uses a throwaway test identity |

Nomen's answer about a token is reused for one minute, so signing out of Nomen reaches the
hub within a minute and a Nomen blip does not interrupt a session already confirmed. Agent
personal access tokens are issued for Nomen's own API, so the hub does not accept them;
agents get their own identity through token exchange in a later increment.

## Tools

`whoami` returns the proven account ID, the login name, whether it is the owner, and the
brokers it may use. `operation_start`,
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

mcp.shippin.cloud passing your Nomen token through to the hub (D2), approving a new
machine from your phone after Shippin sign-in, grants read from the console's staff roles
and server registry once they are live, agent identities by token exchange, installer
autostart for the link, a web UI, the agent-hub board (H3), harness profiles (H4), the
bundle (H5).
