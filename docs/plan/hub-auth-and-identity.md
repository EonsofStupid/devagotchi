# Hub auth and identity

Plan for Devagotchi's own accounts and its own OAuth authorization server. Nomen and shippin.cloud are other products. They may be offered as "sign in with" and nothing more. The hub must serve with none of them configured.

This file is the design the implementation follows. It is grounded in the tree as of the commit that added the verdict. Production `hub.json` is not in the repository.

## Verdict: salvage, partial rewrite, or restart

**Salvage.** Do not restart. Rewrite the hub's identity edge, and leave the machine plane alone.

### Solid and reusable

The product that already works is the broker, the link, and the board. None of that knows who Nomen is.

- One binary, broker owns jobs, hub only routes (`crates/hub/src/lib.rs:1-6`, `crates/hub/src/hub.rs:239-317`). A routed call is an audited IPC or link request. The broker never sees a Nomen token.
- Machine enrollment, owner-scoped devices, one-time codes (`crates/hub/src/devices.rs:172-194`, `crates/hub/src/hub.rs:226-230`).
- `/join` and the downloads directory (`crates/hub/src/join.rs`, `crates/hub/src/downloads.rs`, `crates/hub/src/lib.rs:342-360`).
- Boards, org directories, seats, free limit, approvals (`crates/hub/src/board/mcp.rs:161-168`, `crates/hub/src/board/mcp.rs:202-213`).
- Hash-chained audit (`crates/hub/src/audit.rs:40-88`). `/healthz` already refuses a broken chain (`crates/hub/src/lib.rs:668-688`).
- Session sealing (AES-256-GCM, `__Host-` cookies) (`crates/hub/src/web/sessions.rs:1-27`). The crypto is worth keeping. What is sealed today is a Nomen token (`sessions.rs:28-35`), so the *contents* change.
- Web Push (`crates/hub/src/push.rs`).
- CI: locked build, nextest on six OS targets, `tests/e2e.py`, host install, `cargo deny`, cargo-dist archives attested on `main` (`.github/workflows/ci.yml`, `.github/workflows/release.yml:41-49`).
- Installer and rollback by re-running with the previous `--version` (`deploy/linux/README.md:26`, `deploy/linux/install-devagotchi.sh:433-451`).

That is the Desktop Commander-class product: a program runs on the person's machine, under a grant, with an audit row. Identity is the edge in front of it.

### Coupled to Nomen

The hub is not an account system. `Subject.id` is the identity provider's `sub` (`crates/hub/src/auth.rs:21-36`). The README states the consequence: "The hub keeps no copy of who owns what; it asks Nomen on every new token" (`crates/hub/README.md:143`).

Every authorization decision compares that `sub` to strings in `hub.json`:

| Decision | Where the `sub` is the key |
|---|---|
| Owner reaches every broker | `config.rs:419-422`, required for Nomen mode at `config.rs:237-241` |
| Grants | `config.rs:166-178`, `config.rs:465-476` |
| Human on a board | `config.rs:439-442` |
| `whoami` | `hub.rs:96-119`, `hub.rs:329-366` |
| Account-board directory | `board/mcp.rs:164-168` (`org-<id>` or the `sub`) |
| Device owner | `devices.rs:193-194`, `hub.rs:226-230` |
| Push subscription | `devices.rs:184-189` (`subject`) |
| Audit row | `audit.rs:43`, `hub.rs:261-266` |
| Outage cache | `nomen/kept.rs:42-49` |

The resource server points clients at Nomen, not at itself:

- `401` and both well-known routes (`lib.rs:490-538`, `lib.rs:612-630`). `authorization_servers` is `verifier.issuer()`, which for Nomen mode is `auth.issuer` (`auth.rs:179-185`).
- Scopes are Zitadel audience URNs built from `auth.audiences` (`auth.rs:41-47`).
- Nomen mode never fetches JWKS. It introspects with the hub's API key (`nomen.rs:1-13`, `nomen.rs:478-497`). A `400` from Nomen becomes `issuer.last_error` and a `503` for every new bearer (`nomen.rs:494-497`, `lib.rs:528-536`).
- The public AI client is created as an opaque-token app (`connect.rs:166-184`, `OIDC_TOKEN_TYPE_BEARER`).
- `devagotchi login` is hardwired to Nomen: `DEFAULT_ISSUER` is `https://id.shippin.cloud`, `DEFAULT_CLIENT_ID` is `devagotchi-cli@shippin` (`account.rs:47-50`, `account.rs:545-595`). `connect-nomen` depends on that device flow (`connect.rs:78-88`). Leave `Issuer` in place for `connect-nomen`; change the default of the `login` command.
- `trusted_issuers` accepts another issuer's JWT, still keyed by that token's `sub` (`auth.rs:64-71`, `config.rs:20-25`). It does not create or find a Devagotchi account.
- The website is a different database. `profiles` is keyed by the ChatGPT user id with a unique email (`apps/website/db/schema.ts:4-12`, `apps/website/db/repository.ts:37-45`, `apps/website/db/repository.ts:86-95`). Nothing in the website talks to the hub. There is no Supabase client in `apps/website`. Supabase appears in the vendored Desktop Commander remote client (`reference/DesktopCommanderMCP`), not in this product's site.

`397051786973675523` is not in the source. The live metadata advertises it because `sign_in_scopes` copies `auth.audiences` out of the deployed file. That is a symptom. The design bug is that a Devagotchi account cannot exist without those ids.

### Code quality and coverage

The Nomen resource-server code is careful: fail-closed config (`config.rs` `deny_unknown_fields`), no token in logs, hashed sign-in cache, owner-only key file, tests that stand up a fake Nomen (`crates/hub/src/nomen/tests.rs`, `tests/fake_nomen.py`, `tests/nomen_probe.py`). Keep that discipline.

Coverage of a Devagotchi user is zero, because the type does not exist. `email_verified` is not parsed (`account.rs:234-241` has `email` and no verified flag; `nomen.rs:91-110` has no email at all). Two people with the same verified address cannot be linked, and an unverified address cannot be refused, because neither is stored.

`docs/security-baseline.md:16` says bearers are JWTs checked against Nomen's JWKS. That describes `OidcVerifier` (`auth.rs:311-351`), which Nomen mode does not use. Line 21 still says opaque-token introspection is planned. It is shipped, in `nomen.rs`. The baseline is behind the code.

### Effort

| Option | What it actually is | Why |
|---|---|---|
| **Salvage (this work)** | New store and OAuth server in `crates/hub`, re-key the edges that store a `sub`, point metadata at the hub, change `login`'s default. Broker, link, board, audit, join, push crypto, CI, and the installer stay. | The machine plane is the product and it is already tested on real OSes. Replacing it to get accounts would throw away increments 13 through 33. |
| Partial rewrite of Nomen mode in place | Make `auth.mode: nomen` secretly create Devagotchi users and keep requiring `key_file` and project ids. | Preserves the hard dependency. Existing probes keep passing and the product is still a Nomen app. Rejected as the end state. Nomen mode stays as a legacy verifier so those probes keep passing while `devagotchi` mode is the one that serves. |
| Restart | New broker, new board, new link, new installer, then accounts. | No identity benefit. The unsolved part is the edge, and it is a few modules, not the workspace. |

Salvage is the larger *design* change and the smaller *code* change. The risky pieces are re-keying (a grant string must keep meaning the same person) and the authorization server (PKCE, redirect match, refresh reuse). Both are testable without Nomen.

## How a connector signs in today

`devagotchi hub serve` reads `hub.json` once (`lib.rs:321-361`) and builds a `Verifier` (`auth.rs:92-127`). `/mcp` requires a bearer (`lib.rs:477-484`).

1. No token: `401` with `resource_metadata` pointing at `{public_url}/.well-known/oauth-protected-resource` even for `/mcp` (`lib.rs:501-517`).
2. Both well-known paths share one handler. `resource` is `public_url` with no path (`lib.rs:612-620`). `authorization_servers` is Nomen. `scopes_supported` is `sign_in_scopes(auth.audiences)` (`auth.rs:41-47`, `lib.rs:624-628`).
3. Claude runs authorization-code + PKCE against Nomen. `connect-nomen` registers `devagotchi-ai-connectors` with the claude.ai and claude.com callbacks and opaque access tokens (`connect.rs:31-37`, `connect.rs:166-184`). The hub does not implement Dynamic Client Registration. `docs/security-baseline.md:17` marks Nomen DCR as planned. `/docs` tells the person to paste client id `396558926876835844` (`web/public.rs:36-40`). That constant is not an input to the verifier.
4. The hub introspects (`nomen.rs:358-414`). `assess` requires `active`, issuer, audience, client id, expiry, and a `sub` that passes `valid_subject` (`nomen.rs:616-668`, `config.rs:484-501`). Emails are rejected as account ids on purpose (`config.rs:484-486`).
5. `Hub::subject` reads that `Subject` (`hub.rs:190-205`). `brokers_for` / `is_owner` decide the route (`hub.rs:207-236`). Unknown subject, valid token: authenticated, granted nothing, no board until `board_bind`, machines only after they enroll one.

There is no user table. Stores under the state dir are audit, sign-ins (token hashes), sealed browser sessions, devices, and account-board files. The CLI's `account.json` holds an email from userinfo and the hub never reads that file (`account.rs:8-9`, `account.rs:69-85`).

## What we are building

Devagotchi accounts live in the hub's state directory. An MCP client talks OAuth to the hub. The hub issues its own tokens. An upstream provider is a way to prove an email and a subject, then the hub decides which Devagotchi account that is.

```text
Claude  --OAuth-->  Devagotchi hub (authorization server + resource server)
                         |
                         +-- local magic link (optional, proves the email out of band)
                         +-- "Sign in with" Nomen / shippin / GitHub / Google / website
                         |
                    identities.sqlite3
                         |
                    Subject.id = Devagotchi account id
                         |
                    grants, devices, boards, audit  (unchanged shapes, new ids)
```

### Identity store

`{state}/identities.sqlite3`. Same SQLite habits as audit and devices: WAL, `synchronous=FULL`, `busy_timeout`, immediate transactions, a `schema_migrations` row.

- `accounts(id, display_name, local_email, created_at)`. `id` is `dg_` plus 32 hex characters. `local_email` is set only after a magic link is redeemed.
- `workspaces(id, name, kind, created_at)`. Auto-create one `personal` workspace.
- `memberships(workspace_id, account_id, role)` with role `owner` or `member`.
- `linked_identities(issuer, sub, account_id, email, email_verified, linked_at)`, primary key `(issuer, sub)`.
- `verified_emails(email, account_id)`, primary key `email`. A verified address belongs to one account. Two identities on that account may both carry it.
- `identity_events` append-only: `create`, `link`, `unlink`, `email_conflict`.

Resolution, in one transaction, retried when a unique key loses a race:

1. `(issuer, sub)` already linked: that account. Do not move it if the token's email belongs to someone else. Record `email_conflict` and keep the existing account.
2. Else if `email_verified` and the address is in `verified_emails`: insert the identity onto that account. Event `link`.
3. Else: create account, personal workspace, membership `owner`, identity row. Store the email in `verified_emails` only when `email_verified` is true. Event `create`.

Unverified email never selects an account and is not written to `verified_emails`. Issuer strings are compared without a trailing slash. Email is trimmed and lowercased. `sub` must pass `valid_subject`.

Unlink deletes one identity. It fails when that would leave the account with no linked identity and no `local_email`.

Nomen mode does not open this store. Existing probes keep seeing `Subject.id` equal to the Nomen `sub`. `auth.mode: devagotchi` is the mode that does.

### Re-key

`hub.json` is not rewritten. At start, in devagotchi mode, the hub maps config subjects onto account ids and remembers the map in `linked_identities`:

- `dg_…` that already exists, or that we create so the file's owner has an account.
- `https://issuer|sub` (issuer contains no `|`).
- A bare `sub`, linked under `auth.legacy.issuer` when that is set. Without it, the bare string is linked under `urn:devagotchi:legacy` and will not match a later Nomen login until the operator sets the issuer or links the identity. The deployed owner is a bare Nomen `sub`, so Jesse sets `legacy.issuer` to `https://id.shippin.cloud` once.

Grants are interpreted through that map: `brokers_for` matches when the grant's subject *or its account id* is the caller. Devices and push subscriptions whose `owner` / `subject` equals a mapped `sub` are rewritten once to the account id. Account-board directories named by that `sub` are renamed once. Sealed browser sessions still hold Nomen tokens; they are not rewritten. Those browsers sign in again. The hash chain is not rewritten; old rows keep the `sub` they recorded.

`owner` in the file may be a `dg_` id or `issuer|sub`. `is_owner` is true for the resolved account id.

### Authorization server

The hub is the authorization server for its own resource. Metadata:

| Document | Contents |
|---|---|
| `/.well-known/oauth-authorization-server` (RFC 8414) | issuer = `public_url`, authorize, token, register, revoke, JWKS, device authorization. `code_challenge_methods_supported`: `S256` only. |
| `/.well-known/oauth-protected-resource` | `resource` = `public_url`, `authorization_servers` = `[public_url]` |
| `/.well-known/oauth-protected-resource/mcp` | `resource` = `{public_url}/mcp`, same authorization server |
| `POST /mcp` without a bearer | `resource_metadata` is the `/mcp` document |

`POST /register` (RFC 7591) accepts a public client (`token_endpoint_auth_method: none`) whose redirect URIs are https, or loopback http in tests. It returns a `client_id`. No client secret is required for the MCP clients Claude, ChatGPT, and Cursor register. Redirect URIs are stored and matched exactly.

`GET /authorize` requires `response_type=code`, PKCE `S256`, `state`, and a redirect URI registered for that client. The person signs in (upstream or magic link), sees a consent page naming the client, and posts Allow or Deny. Allow redirects to the client with `code` and `state`. Deny redirects with `error=access_denied`.

`POST /token` accepts `authorization_code` only with the verifier that matches the challenge, and `refresh_token`. Refresh rotates: the presented token is marked used, a new one is issued, and presenting a used token revokes that refresh family. Access tokens are ES256 JWTs, `iss` = `public_url`, `sub` = the Devagotchi account id, `aud` contains `{public_url}/mcp`, `exp` about an hour, `jti` set. The signing key is a P-256 key born in the state dir (0600). JWKS is published. A key older than 90 days rotates; the previous public key verifies until access tokens signed with it have expired. Refresh tokens and authorization codes are stored as SHA-256 only.

`POST /revoke` revokes a refresh token or denies a `jti` until its `exp`.

The resource server checks these JWTs locally. It does not call Nomen to admit them. A JWT from a configured upstream is accepted only as a migration bridge: verify with that issuer's JWKS, then run resolution, and the `Subject.id` is the Devagotchi account id. Opaque upstream tokens are not API bearers.

The CLI's pre-registered public client `devagotchi-cli` uses the device-authorization grant (RFC 8628) against the hub. `devagotchi login` does that by default, hub URL `https://app.devagotchi.com`. `--issuer` remains for `connect-nomen` and for a person who still wants a direct Nomen device flow; it is not the default.

### Upstream providers and local sign-in

```json
"auth": {
  "mode": "devagotchi",
  "legacy": { "issuer": "https://id.shippin.cloud" },
  "upstream": [
    {
      "name": "nomen",
      "issuer": "https://id.shippin.cloud",
      "client_id": "…",
      "scopes": "openid email profile"
    }
  ],
  "local": { "magic_link": true, "deliver": "log" }
}
```

`upstream` may be empty. Each entry is generic OIDC: discovery, authorization-code + PKCE, then `sub`, `email`, `email_verified` (or the provider's equivalent). Nomen, a shippin.cloud issuer, GitHub, and Google are rows in this list, not code branches. GitHub's user email is treated as verified only when the provider says the address is verified.

Magic link: the token is stored hashed, 15 minutes, single use. The HTTP response never contains the link (that would not prove the address). Delivery `log` writes it to the hub journal and to `{state}/login-outbox/` (0600) so a test and an operator can see it. On a public `https` URL, `deliver` defaults to `off` and magic link is refused until the operator sets `log` knowingly, or until real mail exists. Loopback defaults to `log`, which is what CI uses. There is no SMTP client in the hub today; pretending otherwise would be a lie.

The account page lists linked sign-ins, starts another upstream link, and unlinks one that is not the last way in.

### Website

`profiles.id` today is the ChatGPT user id (`repository.ts:91-94`). Add nullable `devagotchi_account_id`. A small pure helper decides: same verified email as a hub account links; a hub `sub` already stored on the profile returns that id; a different verified email does not overwrite. The site documents signing in through the hub's authorization server (the site is a registered client) and writing the resulting `sub` onto the profile. The sanctuary's ChatGPT session is not by itself a Devagotchi login.

### Health

`/healthz` keeps `ok` for brokers and the audit chain, and adds:

- `auth.identity_store` and `auth.signing_key`
- `auth.upstreams[]` with discovery reached or a clear error
- `auth.degraded` listing what is down
- `auth.ready` true only when the store is open, a signing key exists, and every configured upstream discovered (an empty upstream list is ready)

A down upstream is HTTP 200 with `ready: false` and the error text. A store or signing key that will not open is HTTP 503. Nomen being unconfigured is not an error.

### Deploy

Not performed from this work. `docs/deploy.md` and `deploy/linux/promote-hub.sh` describe the pipeline: only a cargo-dist artifact whose commit is on `main` (attested, the same rule as `scripts/lay-down-dist.sh`); install that version beside the current one on a staging host; smoke `/healthz` (`auth.ready`), both metadata documents, and a loopback-style note that the full PKCE connect is the CI probe rather than a production login that would create a person; on success install the same version on the production target; on failure reinstall the previous version (`deploy/linux/README.md:26`). Host, user, and container are environment variables. Production is the hub already on Texas: Proxmox host `tx-infra-prod` (public `23.156.136.173`), CT 110, `https://app.devagotchi.com` behind Cloudflare. Staging is a second container on that same host, its own state directory and port, `https://staging.app.devagotchi.com`. The script does not SSH until `DEVAGOTCHI_STAGING_HOST`, `DEVAGOTCHI_PROD_HOST`, and `--confirm` are set, and it does not embed those addresses.

`homelab-infra-dev` and `wi-infra-dev` do not occur in the tree. `tx-infra-prod` is a broker name in a test fixture (`connect/tests.rs:231`). They are machines an owner grants. A new account does not receive them.

### Comparison

Desktop Commander local is the OS user: stdio to a process on that machine, no account server. Its remote device client does OAuth device authorization against *Desktop Commander's* service and stores a session file mode 0600 (`reference/DesktopCommanderMCP/src/remote-device/README.md`). Supabase is their transport, not the user's identity provider. Claude's custom connector, in this hub's own `/docs`, already expects an OAuth server in front of the MCP URL. The usual shape for a remote MCP product is: the product is the authorization server, external login is a button, and the token the client stores is the product's. Pointing Claude at Nomen and keeping no user row is the inversion. ChatGPT's plugin OAuth and Cursor's MCP OAuth are the same pattern: protected-resource metadata, an authorization server that speaks PKCE, dynamic registration when the client does not have a pre-issued id.

## Alternatives rejected

| Alternative | Why not |
|---|---|
| Keep resolving only at request time against Nomen, store nothing | The next project restore breaks every connector, which is the outage this plan exists to end. |
| Use the Nomen `sub` as the Devagotchi id forever | The account cannot survive a second issuer, and the product does not exist when Nomen is removed. |
| Auto-grant `tx-infra-prod` to a new account | Those machines are Jesse's. A new account gets a personal workspace and the machines they enroll. |
| Link on unverified email or on a display name | `valid_subject` exists because both have been shared (`config.rs:484-486`). |
| Implement DCR by proxying Nomen's registration | Nomen DCR is not built (`security-baseline.md:17`), and it would leave Nomen as the authorization server Claude talks to. |
| Show the magic link in the HTTP response | The person who typed the address would be signed in without controlling the mailbox. |
| Rewrite `auth.mode: nomen` to return Devagotchi ids | Breaks every probe that asserts `whoami.subject` equals the fixture `sub`, for a mode we are demoting. Legacy mode stays. |
| A new crate or a second database service | The state dir is the hub's store. `deny.toml` already confines HTTP clients to the hub. |

## Phases

Each phase is one commit, tests green before the next. Nomen-mode tests stay green throughout.

1. `crates/hub/src/identity.rs` — store, resolution, events, unlink. Unit tests: new account, existing `(issuer, sub)`, verified-email link, unverified does not link, second issuer same email links, existing link wins over a conflicting email, unlink of the last way in fails, concurrent creators collapse to one account.
2. Re-key. `AuthConfig::Devagotchi`. Startup map for `owner` and grants. Devices, push subscriptions, board directories. Tests with a fixture `sub` and a `dg_` owner.
3. `crates/hub/src/oauth.rs` — keys, DCR, authorize (session required), consent, token, refresh rotation, revoke, JWKS, metadata, local JWT check on `/mcp`. Tests cover PKCE failure, redirect mismatch, refresh reuse, and a JWT admitted while introspection is not configured.
4. Upstream OIDC list and magic link. Account page: list, link, unlink. Fake upstream in tests. Loopback outbox for the link.
5. `devagotchi login` device flow against the hub by default. `account.json` shape unchanged (`Account` already has issuer, client id, subject, email, tokens). `connect-nomen` still uses `Issuer`.
6. Website helper and `devagotchi_account_id` column. `docs/auth.md` describes the site bridge. No live call to a hub from the website test suite.
7. `/healthz` `auth` object.
8. `tests/oauth_probe.py` wired from CI: DCR, authorize through a fake upstream, token, MCP `initialize`, `whoami`, for a new user, a returning user, and a second issuer with the same verified email. `docs/deploy.md` and the promote script. No deploy.
9. Hub README, `docs/auth.md` if not already enough, `CHANGELOG.md`, `docs/security-baseline.md` rows 16-17 and 21 so they match the code.

## Agent-first onboarding

The product goal is that an AI can open an account for the person and keep going. A human may tap once. They do not create an account, paste a client id, or edit `hub.json` before the first machine works.

What already existed, and what was missing:

- A machine enrolled with an account's own code is reachable by that account with no grant (`hub.rs` `route`: configured brokers use `brokers_for`; any other name is allowed when `device.owner` is that account). Operator brokers stay grant-only.
- `POST /api/v1/machines` already mints that code, but only for a bearer that already exists (`web/machines.rs`).
- OAuth and device login both waited for a human to finish an upstream or magic-link sign-in before any token existed. That is the dead end.

### (a) claude.ai, ChatGPT, Cursor

1. The client fetches `GET /.well-known/oauth-protected-resource/mcp`. `resource` is `{public_url}/mcp`. `authorization_servers` is this hub.
2. `POST /register` with the client's redirect URI. Response is `client_id`. No secret.
3. The client sends the person to `GET /authorize` with `response_type=code`, PKCE `S256`, `state`, and that `redirect_uri`.
4. The page is one button, Allow. `POST /authorize/provisional` creates a provisional Devagotchi account and a personal workspace, records the client as an agent seat, consents, and redirects back with `code` and `state`. No upstream login and no magic link on this path.
5. `POST /token` with the verifier. The access token is a hub JWT. `sub` is the new account id. `agent` is the client name. `whoami` includes `provisional_until` and `claim_url`.
6. `tools/call machine_add` `{ "name": "laptop" }` returns the one-line join command (`curl … | sudo sh` or the Windows `irm` line) and a code that expires in 15 minutes. The AI shows it or runs it.
7. The machine enrolls with that code. It is `devices.owner` = the account. `operation_start` on that name is routed. It is not a grant. A name that is already an operator broker is refused.
8. Later, the person opens `claim_url` (also printed by `whoami`) and signs in with any configured provider or a magic link. That identity is attached to this same account. If the verified email already belongs to another Devagotchi account, the provisional account is merged into that one: machines, agents, and tokens follow (`merged_into`), and `whoami` on the old token returns the surviving id.

### (b) Claude Code, Codex, `devagotchi mcp`

1. `devagotchi login` (no flags) calls `POST /auth/agent/start` with `client_id=devagotchi-cli` and `agent` from the environment or `cli`. The hub returns tokens immediately and a claim code. Nothing is polled and nothing is approved in a browser first.
2. The tokens land in `account.json` as they do today. `devagotchi mcp --hub` sends the access token.
3. The agent calls `machine_add` and runs the join line in the local shell. The machine comes online as this account's.
4. The claim code is on the terminal and in `whoami`. The person uses it when they want the account to survive.

`--issuer` is unchanged: it still waits on an upstream device code. That path is for `connect-nomen`, not for an agent opening an account.

### (c) An agent on a fresh machine

The agent already holds a token from (a) or (b), or it calls `/auth/agent/start` itself. `machine_add` is the whole setup. The join script installs the broker and the link and enrolls with the code. No operator edits `grants`. The free owned-machine cap (16, unless the account is the hub owner) still applies.

### How others do it

Desktop Commander's local MCP does not have an account: the OS user is the principal, and the agent talks to a process on that machine. Its remote device client still stops until a person finishes OAuth in a browser and a device session file exists (`reference/DesktopCommanderMCP/src/remote-device/README.md`). Smithery and the usual "connect this MCP" screens stop at the same place: the client cannot call tools until a human completes OAuth or pastes a key. Those flows are right for a shared hosted tool. They are a dead end when the point is the person's own machine and the AI is the one sitting at the keyboard. The provisional account is that difference: the token exists before the identity provider does, the machine enrolls to it, and the human's later sign-in claims the same id instead of starting over.

### Abuse and limits

- Provisional creation is limited to 8 per IP per hour, and to 500 live unclaimed accounts on the hub. Both are refused with `rate_limited` or `provisional_quota`.
- An unclaimed account expires after `auth.provisional.ttl_days` (default 14, range 1..=90). The Shippin control plane changes that, the 8/IP/hour rate, and the 500 unclaimed cap at runtime with `GET`/`PUT /api/v1/admin/settings` (owner, or a grant with `operator: true`; audited). The saved row wins over `hub.json` on restart. `whoami` carries `provisional_until` and `provisional_warning` from the live lifetime. A token for an expired provisional account is `invalid_token`. Claimed accounts do not expire.
- The claim code is single-use, stored hashed, and shown to the agent so it can hand it to the person. It is not a second account.
- Each client name is an agent seat on the account, listed on `/account/identities`, revocable there. A revoked seat's tokens fail verify. Seats are not people and are not grants.
- Owned machines stop at 16 per account. Operator-configured brokers are unchanged: no grant, no route.
- Every create, claim, merge, and enroll is an identity event. Enroll and `machine_add` also hit the hash-chained audit.

## Open questions

1. For the live hub, is the configured `owner` still the same Nomen `sub` as before the project restore? `legacy.issuer` links that string. If the person's Nomen `sub` changed, the grant will not follow until someone links the new `sub` to that account (verified email does that on their next sign-in, once `email` is in the upstream scope).
2. Magic link on `https://app.devagotchi.com` stays off until `local.deliver` is `log` or real mail exists. Is journal delivery acceptable for the alpha, or is an upstream (Nomen or Google) the only public button for now?
3. Should a migrated bare `sub` in a grant keep working before that person has signed in even once? The plan says yes: startup creates the account and the link under `legacy.issuer`.
4. Staging is a second instance on the Texas host (`tx-infra-prod`, public `23.156.136.173`), not CT 110: its own container, state directory, and port, at `staging.app.devagotchi.com`. `docs/deploy.md` names that. This work does not run the promote script.
5. `connect-nomen` still writes Nomen client ids into `hub.json`. After cutover those ids are unused by the resource server. Leave the command as an upstream-setup helper, or stop rewriting `auth`? The plan leaves the command alone so a Nomen *upstream* can still be created with the owner's approval, and stops it from being required to serve `/mcp`.
