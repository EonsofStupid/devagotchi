# Devagotchi identity

One account system, on the hub, that an AI can open and a person can claim. Nomen is a way in, not the account. This is the model for `https://app.devagotchi.com` and for any hub in `auth.mode` `devagotchi`. It is written from the code that is running there (commit `190978ae` on Texas CT 110) and from the passkey work already on `main` (`crates/hub/src/accounts.rs`, `web/passkeys.rs`, `devagotchi hub owner`).

Nothing in this document deploys.

## What is live

Claude.ai already completes OAuth against this hub: dynamic registration of a public client, PKCE S256, the Allow page, a hub-signed JWT, refresh rotation with reuse detection, and the tool list. That path is `crates/hub/src/oauth.rs`. The account id in the token is a Devagotchi id (`dg_` plus 32 lowercase hex), not a Nomen subject (`crates/hub/src/identity.rs`).

The hub that is up has this owner row, and it must survive every change here:

| | |
|---|---|
| Account | `dg_7dae7adce9ba2cc180ed32c1c8bbe7c3` |
| Linked sign-in | issuer `https://id.shippin.cloud`, subject `395290028818825218` |
| Brokers | `homelab-infra-dev`, `tx-infra-prod`, `wi-infra-dev` |

Those broker names stay grant-only. A new account does not receive them. A machine the account enrolls is `devices.owner` of that account (`crates/hub/src/link.rs`).

The code also has `auth.mode hub` (`docs/product-and-platform-plan.md` §14), with `acct_` ids in its own store (`accounts.sqlite3` or Postgres) and the rule that a configured owner keeps the bare Nomen subject as the account id. Both modes are in the code side by side since PR #79 merged (increment 42); a hub runs the one its config names and they share no tables. This hub runs `devagotchi` and already rekeyed that subject onto the `dg_` row above. Switching it to `hub` mode, or rewriting the subject back, would orphan the grants and the machines. `identities.sqlite3` is the account of record on this hub.

## Accounts and workspaces

An account is a row in `accounts`: `dg_` id, optional display name, optional local email, `created_at`, `provisional_until`, `merged_into`. A personal workspace (`ws_` id, kind `personal`) is created with it. Membership role on that workspace is `owner` of the workspace, which is not the hub owner.

A linked sign-in is one row in `linked_identities`, primary key `(issuer, sub)`, pointing at exactly one account. A verified email is one row in `verified_emails`, primary key the normalized address. An unverified email is not stored there and never selects an account.

Resolution when someone signs in (`Store::resolve`):

1. `(issuer, sub)` already linked: that account. If the verified email now belongs to a different account, record the conflict and do not move the identity.
2. Else a verified email already on file: link this `(issuer, sub)` to that account.
3. Else create an account and its personal workspace.

`merged_into` is a tombstone. `canonical` follows it, at most four steps. Tokens keep working because verify uses the canonical id, not only the `sub` baked into an old JWT.

Hub owner is not a workspace role. It is `hub.json` `owner` after rekey, or the account a setup code promotes (below). The owner reaches every configured broker. Everyone else needs a grant, or a machine they enrolled themselves.

## Credentials

A credential proves an account. It is not an account.

| Credential | Who | Where it lives |
|---|---|---|
| MCP access JWT | An agent seat on the account | Signed by the hub (`oauth-signing.json`, P-256, mode 0600). `aud` is `{public_url}/mcp`. About one hour. `jti` can be revoked. |
| Refresh token | That same client | Opaque, stored as SHA-256, family rotation. Reuse of a spent token deletes the family. |
| Passkey | The person | WebAuthn, attestation `none`, verified in-process with `aws-lc-rs` (the verifier shape in `crates/hub/src/accounts/passkey.rs` on `main`: ES256, EdDSA, RS256). Public key and sign count in the identity store. Not `webauthn-rs` (it pulls OpenSSL, which `deny.toml` bans). |
| Email magic link | The person | Single-use token, hash only, short TTL. Off on a public URL until `auth.local.deliver` is `log` or real mail exists. |
| Recovery codes | The person | Ten single-use codes, hashes only, shown once when the first passkey is enrolled. |
| Nomen (or any OIDC upstream) | The person | Not a credential the hub stores. The hub is a public PKCE client of that issuer. The result is a `linked_identities` row. |
| Setup code | The operator on the host | Hash only, 15 minutes, one outstanding code. |

Browser sessions are `dg_as` cookies: the id is hashed at rest, `HttpOnly`, `SameSite=Lax`, `Secure` on https. An MCP token is not a browser session and is not a person: it carries `agent`, and it cannot force-release a board ticket.

There is no OAuth auto-accept. `/authorize` for a new client is a consent page. Allow is an explicit POST. A provisional account is that POST, or `POST /auth/agent/start` from the CLI, not a silent redirect.

## The hub is the authorization server

`auth.mode` `devagotchi` (`crates/hub/src/oauth.rs`, `crates/hub/src/auth.rs`). Metadata is RFC 8414 on the hub. The protected resource is RFC 9728, resource `{public_url}/mcp`. Registration (`POST /register`) accepts public clients only (`token_endpoint_auth_method` `none`). Redirects are `https`, or `http` to loopback. PKCE is S256 only. The seeded client `devagotchi-cli` is for the CLI.

Nomen mode (`auth.mode` `nomen`) stays for hubs that have not cut over. It is not how `app.devagotchi.com` serves `/mcp`.

## Provisional accounts

An AI must be able to work before a person finishes sign-in.

- Remote connector: `GET /authorize` is one Allow button. `POST /authorize/provisional` creates the account, records the client as an agent seat, consents, and returns the authorization code. No upstream and no magic link on that POST.
- CLI: `devagotchi login` calls `POST /auth/agent/start` and stores the tokens. The terminal prints the claim code.
- `whoami` includes `provisional_until`, `claim_code`, and `claim_url`. `claim_url` is `{public_url}/claim?code=…`.

Limits, live, from `GET`/`PUT /api/v1/admin/settings` (`crates/hub/src/lib.rs`): `provisional.ttl_days` 1..=90, default 14; `starts_per_hour` 1..=10000, default 8; `unclaimed_cap` 1..=1000000, default 500. Caller is the hub owner or a grant with `operator: true`. The saved row in `operator_limits` wins over `auth.provisional` in `hub.json` on restart. The next account uses the new lifetime. Accounts already opened keep the `provisional_until` they were given. `provisional_warning` is recomputed from the live lifetime (under one seventh, and at least an hour). Denied calls and changes are audit rows `admin.settings`.

An expired provisional token is `invalid_token`. A claimed account does not expire.

## Claiming

`GET /claim?code=` is a real page. Today `claim_url` points there and the router has no `/claim` (`oauth.rs` builds the URL; `http_router` never registers it), so the browser gets 404. The page offers every way in that is actually configured, and says so when one is off:

- Passkey, if this browser can use one already enrolled on an account, or to enroll one after the setup code.
- Nomen, or any other `auth.upstream` provider: link to `/login/upstream/{name}?resume=claim:CODE`. The callback is `{public_url}/login/upstream/{name}/callback`. For Nomen that path is `/login/upstream/nomen/callback`.
- Magic link, only when `magic_delivery()` allows it. The resume stored with the link is `claim:CODE`. Redeem must claim. Today `/login/redeem` treats every resume as `/authorize/resume?request=…` (`oauth.rs` around the redeem handler), so a claim code is ignored.
- Setup code, the field on this page.

`POST` of a successful proof calls `Store::claim`. The code is single-use, stored as SHA-256 plus a display copy until it is used (`claim_codes`).

How `claim` lands:

1. Unknown, used, or expired code: `invalid_grant`. No account is created.
2. `(issuer, sub)` is already linked to account S. If S is the provisional account, clear `provisional_until`. If S is another account, **merge the provisional account into S**. Do not `INSERT` a second `linked_identities` row. Today the email branch merges, and the other branch always `insert_identity` (`identity.rs` `claim`, around the `insert_identity` call). That is the primary-key conflict on the live hub: Jesse's Nomen subject is already linked to `dg_7dae7adce9ba2cc180ed32c1c8bbe7c3` with no row in `verified_emails`, so claim tries to insert the same `(issuer, sub)` onto the provisional account and SQLite rejects it. The provisional account stays an orphan for 14 days.
3. Else a verified email already belongs to account E, and E is not the provisional account: merge into E, then link `(issuer, sub)` to E if it is not already there.
4. Else attach the proof to the provisional account and clear `provisional_until`.

Merge (`merge_tx`) moves agents, verified emails, linked identities, and claim codes. It sets `merged_into` and clears `provisional_until` on the absorbed account. `Authority::claim_with` queues the pair so the next request runs `Links::reassign_owner`: devices, enroll codes, and push subscriptions follow. Refresh tokens for the absorbed id are rewritten to the survivor. Grants in `hub.json` are not rewritten; they already name the survivor if rekey ran, because the owner subject was linked at startup.

A magic-link claim uses issuer `urn:devagotchi:magic` and the normalized email as `sub`, with `email_verified`. It goes through the same `claim` rules. It does not call `redeem_local` first: that would create a second account before the merge decision.

The owner can raise the lifetime of unclaimed accounts with the admin settings API. Promoting one provisional account is claiming it, or merging it, not a separate flag.

## Linking Nomen

Nomen is an `auth.upstream` entry, a public PKCE client, scopes including `email`:

```json
"upstream": [{
  "name": "nomen",
  "issuer": "https://id.shippin.cloud",
  "client_id": "<the hub's public client at Nomen>",
  "scopes": "openid email profile"
}]
```

The redirect URI a Nomen admin must allow is exactly:

`https://app.devagotchi.com/login/upstream/nomen/callback`

Discovery, token, and userinfo are the provider's. The hub checks the discovery `issuer` matches the configured issuer. The `sub` from userinfo is the Nomen subject. A verified email links; an unverified email does not select an account.

Signing in with Nomen while holding `resume=claim:CODE` is a claim, not a new account. Signing in with Nomen while already in a browser session links the subject to that session's account (`link_to`) unless the subject already belongs to another account, in which case the claim/merge rule above applies when the resume is a claim code. A plain sign-in whose `(issuer, sub)` is already linked returns that account (`resolve`). It does not insert.

`legacy.issuer` `https://id.shippin.cloud` is how a bare subject already written in `owner` or `grants` was adopted into `dg_7dae7adce9ba2cc180ed32c1c8bbe7c3`. The config file is not rewritten.

## Owner bootstrap

A public URL must not be ownable by whoever loads `/authorize` first.

When the hub has no claimed owner (no `owner` in config, and no account marked owner in the store), `devagotchi hub setup-code` generates a setup code: 15 minutes, SHA-256 at rest, one outstanding code, a new call replaces it. It prints the code once to stdout and logs that same line once. The plaintext is not written into `hub.json` or a world-readable file. Whoever can run that command on the host is the same trust boundary as `devagotchi hub enroll`.

Entering the code on `/claim` or the sign-in page, in the same POST as a new passkey ceremony, makes that account the hub owner and enrolls the passkey. The code is spent in that transaction. A second browser that loaded the page without the code does not become owner.

A hub that already has an owner, including this one, does not mint a setup code that could replace them. The owner takes a provisional agent account by signing in with the linked Nomen subject (the merge in Claiming) or with a passkey already on `dg_7dae7adce9ba2cc180ed32c1c8bbe7c3`. `devagotchi hub owner` from `main` (one-time link, 15 minutes, `--replace` to drop other passkeys) is this same operation once it writes the live store: run on the host, it adds a passkey to the existing owner and ends that owner's other sessions. It does not create a second owner account and it does not change `dg_7dae7adce9ba2cc180ed32c1c8bbe7c3`.

## Sessions, clients, consent

Every MCP client is a row from dynamic registration or the seeded CLI client: `client_id`, name, redirect URIs. Consent is per client: the person, or the one Allow on a provisional account, agrees to that `client_id` and those scopes. A later client is a new consent, not a reuse of the first Allow.

The owner page lists:

- Linked sign-ins, with unlink. Unlink refuses the last way in (a linked identity, a passkey, or a local email).
- Agent seats, with revoke. A revoked seat fails verify.
- OAuth clients that hold a grant for this account, with revoke. Revoke deletes that client's refresh family and denylists outstanding access `jti`s.
- Browser sessions, with revoke.

Each of those writes the hash-chained audit (`audit.sqlite3`) and an `identity_events` row. The audit does not contain tokens, claim codes, or setup codes.

## Machines

A join code is `dvg-` plus 32 lowercase hex, 15 minutes, single-use, bound to the account that called `machine_add` or the web form (`devices.rs`). Fetching a script does not spend it. Enroll spends it. The code is not a shell argument; the script passes it on stdin or in the environment.

`machine_add` and the web UI return every way to use that one code:

| Way | What the person or the AI runs |
|---|---|
| Linux | `curl -fsSL {hub}/join/{code} \| sudo sh` — systemd user units, root once for the install |
| Windows | `irm {hub}/join/{code}.ps1 \| iex` — current-user install, no administrator |
| Docker Compose | A compose file from `{hub}/join/{code}.compose.yml` that runs the same enrollment against a volume for the state directory |
| Proxmox or Incus host | `{hub}/join/{code}.host.sh` — the Linux installer on the host itself, not inside a guest, so the broker is the host's |
| Manual | The tarball URL under `{hub}/dist/…`, the checksum, and `devagotchi link enroll` with the code on stdin |

`/join` serves the script only when `/dist` has the archive and `SHA256SUMS` for this hub's version (`crates/hub/src/join.rs`). A pull-request commit has no GitHub Release, so a hub installed from a PR keeps serving whatever was copied into the downloads directory last. Promoting a build copies that build's archives into the downloads directory as part of the install. The script refuses a checksum mismatch and does not enroll.

Operator-configured brokers are not issued by `machine_add`. The free owned-machine cap stays 16 unless the account is the hub owner.

## Threat model for a public URL

The hub listens on loopback. Cloudflare and Caddy are the TLS edge. The hub trusts the bearer token and the browser session, not the network.

| Threat | What holds |
|---|---|
| Someone else loads `/authorize` and clicks Allow | They get their own provisional account, not the owner's brokers. Claiming into the owner requires the owner's Nomen subject, passkey, or the setup code. |
| Race to become owner | No owner is created by the Allow button. The setup code is hashed, short-lived, and only issued on the host. |
| Stolen provisional token | 14 days or the configured lifetime, one agent seat, no configured brokers, rate limited per IP, capped unclaimed accounts. Revoke the seat. |
| Stolen refresh token, then the real client retries | Reuse deletes the family. |
| Claim code in a chat log | Single-use. Merges into the person who can prove a linked identity, which the thief does not have. After use the display copy is cleared. |
| Setup code in the journal | One line, at generation, on the host journal. The same people can already run `hub enroll`. Regeneration replaces it. |
| Nomen down | Passkey, magic link (if mail is on), and existing MCP tokens still work. The hub does not call Nomen to check its own JWTs. |
| Open redirect | Redirect URIs are the client's registered set: https or loopback http, no fragment. Upstream `state` is stored here and checked once. |
| PKCE downgrade | `plain` is rejected. |
| Email squat | Unverified email does not select and is not stored in `verified_emails`. |
| Last way in removed | Unlink and passkey removal refuse when nothing else can sign in. |
| Join script injection | Every value written into a script matches a fixed pattern and is single-quoted (`join.rs`). |
| Two hubs, one state directory | Staging is a different container. This hub's state directory is not shared. |

## Migrations

Schema versions already in `identities.sqlite3`:

1. Accounts, workspaces, memberships, linked identities, verified emails, identity events.
2. `provisional_until`, `merged_into`, agents, claim codes.
3. `operator_limits` (the admin settings row).

The next version adds, without rewriting existing rows:

- Passkey credentials (id, account, public key, algorithm, sign count).
- Recovery-code hashes.
- Setup-code hash, expiry, used.
- An owner mark on the account, set only by the setup code or by recognizing the rekeyed `owner` at startup. For this hub that mark is `dg_7dae7adce9ba2cc180ed32c1c8bbe7c3`. It is not a new id.

`oauth.sqlite3` (clients, codes, refresh families, signing keys) stays. `devices.sqlite3` stays; `reassign_owner` is the merge path for machines. `audit.sqlite3` stays append-only. `hub.json` is not rewritten by claim or by rekey.

A database that only has `main`'s `accounts.sqlite3` is a hub that never ran this branch. This document does not convert that file on the Texas hub, because that file is not the live store. New passkey rows go into `identities.sqlite3`.

## What a Nomen admin does

Create or reuse a public PKCE client (no secret) whose redirect URI is:

`https://app.devagotchi.com/login/upstream/nomen/callback`

Put its client id in `auth.upstream` as above. Scopes must include `email` so a verified address can merge. Do not put that client id in `auth.audiences`. The resource server does not ask Nomen to validate hub JWTs.

## What Jesse runs on CT 110

After a build of this model is installed, and only then:

```text
sudo devagotchi hub setup-code \
  --state-dir /var/lib/devagotchi/.local/state/devagotchi-hub \
  --config /etc/devagotchi-hub/hub.json
```

On this hub the owner is already set. The command prints the Devagotchi account id (the `dg_` id the Nomen subject `395290028818825218` is linked to, `dg_7dae7adce9ba2cc180ed32c1c8bbe7c3`) and `claim https://app.devagotchi.com/claim`. It does not print the Nomen subject and it does not mint a code that would create another owner. Jesse opens that claim URL from an agent's `claim_url` and signs in with Nomen. The Nomen button reads "Sign in with Nomen" when `auth.upstream[].display_name` is unset. It does nothing until the redirect URI above is registered and `hub.json` has the upstream entry. Magic link stays off until mail or `local.deliver` `log` is an accepted choice.
