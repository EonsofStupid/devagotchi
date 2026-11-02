# Devagotchi accounts and sign-in

Devagotchi has its own accounts. Nomen and shippin.cloud are optional "sign in with" providers. The hub serves with none of them configured.

This page is `auth.mode` `devagotchi`. The hub has a second account system beside it, `auth.mode` `hub`: passkey accounts in SQLite or Postgres, the hub's own OAuth server for AI clients at `/oauth/*`, and the browser claim of a new hub. That one is described in [crates/hub/README.md](../crates/hub/README.md) and is what `deploy/compose/` runs. A hub runs the mode its config names. The two share no tables, and each mounts its own `/claim` and `/.well-known/oauth-authorization-server`.

The design notes, including how today's Nomen-only hub maps onto this, are in [plan/hub-auth-and-identity.md](plan/hub-auth-and-identity.md).

## What a minimal hub.json is

```json
{
  "listen": "127.0.0.1:8787",
  "public_url": "https://app.devagotchi.com",
  "auth": { "mode": "devagotchi" },
  "owner": "dg_… or https://id.shippin.cloud|<nomen sub>",
  "brokers": [],
  "grants": []
}
```

`auth.mode` `devagotchi` is the authorization server. Claude, ChatGPT, and Cursor register a public client at `POST /register` (dynamic client registration). They do not need a Nomen client id. Protected-resource metadata at `/.well-known/oauth-protected-resource/mcp` names this hub as both the resource (`{public_url}/mcp`) and the authorization server.

Optional providers:

```json
"auth": {
  "mode": "devagotchi",
  "legacy": { "issuer": "https://id.shippin.cloud" },
  "upstream": [
    {
      "name": "nomen",
      "issuer": "https://id.shippin.cloud",
      "client_id": "<public client id at Nomen>",
      "scopes": "openid email profile"
    }
  ],
  "local": { "magic_link": true, "deliver": "off" }
}
```

`legacy.issuer` is how a bare subject already written in `owner` or `grants` (a Nomen `sub` from the previous config) becomes a stable Devagotchi account. The file is not rewritten. The map is remembered in `{state}/identities.sqlite3`. Machines enrolled to that subject move with it. Restarting does not mint a second account.

A new person gets an account id `dg_` plus 32 hex characters and a personal workspace. Their first sign-in:

1. `(issuer, sub)` already linked → that account.
2. Else a verified email already on file → link this subject to that account.
3. Else create an account. An unverified email never selects an account.

`devagotchi login` (no flags) does device authorization against the hub, default `https://app.devagotchi.com`. `--issuer` still talks to an upstream issuer directly. `connect-nomen` keeps using that direct path. `--hub URL` points the device flow at another hub.

## Magic links

A magic link proves the mailbox only if it is delivered out of band. The HTTP response never contains it. On a public `https` URL, magic links are off until `auth.local.deliver` is `"log"`. `log` writes the URL to the hub journal and to `{state}/login-outbox/` (mode 0600). Loopback defaults to `log`, which is what tests use. There is no mail sender in the hub.

## Tokens

Access tokens are ES256 JWTs signed by a P-256 key in `{state}/oauth-signing.json` (mode 0600). The hub checks them locally. `aud` is `{public_url}/mcp`. Refresh tokens rotate; presenting a used refresh token revokes that family. Authorization codes require PKCE S256.

`auth.mode` `nomen` and `oidc` still work as they did. They do not create Devagotchi accounts. A hub that should have accounts of its own uses `devagotchi` or `hub`.

## Health

In `devagotchi` mode, `GET /healthz` includes `auth` (other modes leave it out):

| Field | Meaning |
|---|---|
| `identity_store` | The account database opened |
| `signing_key` | A signing key is loaded |
| `upstreams[]` | Each provider's discovery document, or the error |
| `degraded` | What is down, in words |
| `ready` | Signing key present and every configured provider discovered. An empty provider list is ready |

A down provider does not by itself fail the process. A missing signing key does.

## Website

devagotchi.com profiles are a separate table, keyed today by a ChatGPT user id, with a unique email. They become the same Devagotchi account when someone signs in through this hub and the token's verified email matches the profile. See `apps/website/lib/hub-account.ts`. An unverified or different address does not link. An account id already stored on the profile is kept.

## Agent-first onboarding

An AI can open the account and reach the person's own machine before anyone finishes a sign-in. Securing the account is a later claim, one code, same account id.

### Remote connector (claude.ai, ChatGPT, Cursor)

1. `GET /.well-known/oauth-protected-resource/mcp` names this hub.
2. `POST /register` returns `client_id`.
3. `GET /authorize` shows one Allow button. `POST /authorize/provisional` creates a provisional account and returns the authorization code. No upstream and no magic link.
4. `POST /token` returns a hub JWT.
5. `machine_add` `{ "name": "laptop" }` returns `curl -fsSL {hub}/join/{code} | sudo sh` and the Windows line. The AI runs it or hands it over.
6. The machine enrolls to that account. `operation_start` with `broker` set to that name is routed. Operator brokers in `hub.json` still need a grant.
7. `whoami` includes `claim_url` and `provisional_until`. The person opens it and signs in. The identity attaches to this account. A verified email that already belongs to an account merges this provisional one into that account; machines and the old token follow.

### Local agent (Claude Code, Codex, `devagotchi mcp`)

`devagotchi login` calls `POST /auth/agent/start` and stores tokens immediately. The terminal prints the claim code. The agent then calls `machine_add` and can run the join line itself. `--issuer` is the old device flow and is not this path.

### A fresh machine

The join line from `machine_add` is the whole install. The new account owns what it enrolls. Nobody edits grants for that machine.

### Limits

Eight provisional accounts per IP per hour. Five hundred unclaimed accounts on the hub. Unclaimed accounts last 14 days and then their tokens stop. `whoami` carries `provisional_until`. When the time left is under one seventh of the live lifetime (at least an hour), `whoami` also sets `provisional_warning`. That warning follows the current setting, not the lifetime the account was created with. A claimed account does not expire.

`auth.provisional` in `hub.json` is the starting value (`ttl_days` 1..=90, default 14; `starts_per_hour` 1..=10000, default 8; `unclaimed_cap` 1..=1000000, default 500). It applies only until an operator saves a row.

The Shippin control plane changes the live values without a restart:

```
GET /api/v1/admin/settings
PUT /api/v1/admin/settings
{"provisional": {"ttl_days": 14, "starts_per_hour": 8, "unclaimed_cap": 500}}
```

Bearer token of the hub `owner`, or of a grant with `"operator": true`. Anyone else is 403. A missing or bad token is 401. `ttl_days` outside 1..=90, or the other two fields outside the ranges above, is 400 and the previous value stays. A field left out of `provisional` keeps its current value. The next account uses the new lifetime immediately. Accounts already opened keep the `provisional_until` they were given. The saved row is in the hub state directory and wins over `hub.json` on the next start. Denied calls and every change are on the hash-chained audit as `admin.settings`.

Each AI client is a seat on `/account/identities` and can be revoked. Create, claim, merge, and enroll are audited.

## What Jesse sets on app.devagotchi.com

1. Install a build whose commit is on `main` (see [deploy.md](deploy.md)). Do not hand-edit project ids into `auth.audiences`.
2. `auth.mode` `devagotchi`. `public_url` `https://app.devagotchi.com`. Optional `auth.provisional` overrides the 14-day / 8-per-hour / 500-unclaimed defaults until the first `PUT /api/v1/admin/settings`. After that, the control plane's saved row is what restarts use. Give the control plane's account a grant with `"operator": true`, or call as `owner`.
3. `legacy.issuer` `https://id.shippin.cloud` if `owner` and grants are still Nomen subs. Or replace `owner` with `https://id.shippin.cloud|<sub>` or with the `dg_` id `whoami` prints after the first sign-in.
4. Optional: one `upstream` entry for Nomen (or Google, or anything with a discovery document), public client, scopes including `email`.
5. Leave `local.deliver` off on the public URL until mail exists, unless journal delivery is an accepted alpha.
6. Restart the hub user's service: `systemctl --user restart devagotchi-hub`.
7. `GET /healthz` and read `auth.ready` and `auth.degraded`. A connector URL is `https://app.devagotchi.com/mcp`. The client registers itself. No pasted Nomen client id.

Configured brokers are not granted to a new account. A person reaches a machine they enroll, or a broker an operator grants to their Devagotchi account id.
