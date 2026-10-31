# Hub auth and identity

Plan only. This document describes the hub as it is on `main`, why a claude.ai connector fails against the live hub, and the change that should follow. It does not change the hub.

Grounded in the tree at the commit this file was added. Production `hub.json` is not in the repository; where the live failure depends on it, that is called out.

## How a claude.ai connector signs in today

The hosted hub is one process: `devagotchi hub serve --state-dir … --config …` (`crates/hub/src/lib.rs:321-361`). It reads `hub.json` once at start (`lib.rs:322-328`), builds a `Verifier` (`lib.rs:361`, `auth.rs:92-127`), and serves Streamable HTTP MCP at `/mcp` behind a bearer check (`lib.rs:477-484`, `lib.rs:490-538`).

Claude is told, on the hub's own `/docs` page, to add a custom connector at `https://app.devagotchi.com/mcp` and to paste a client id (`crates/hub/src/web/public.rs:36-40`). That page is static HTML. The client id in it is not read from Nomen and is not the id the verifier accepts.

The connector's steps, as this code answers them:

1. Claude calls `POST /mcp` with no token. `require_bearer` finds no `Authorization` header and returns `401` with `WWW-Authenticate` (`lib.rs:505-517`, `lib.rs:598-609`). The challenge always points at `{public_url}/.well-known/oauth-protected-resource`, including for a call to `/mcp` (`lib.rs:501-503`). In Nomen mode it also sends `scope="…"` built from `auth.audiences` (`lib.rs:495-498`, `auth.rs:41-47`).

2. Claude fetches protected-resource metadata. Both `/.well-known/oauth-protected-resource` and `/.well-known/oauth-protected-resource/mcp` are the same handler (`lib.rs:469-475`, `lib.rs:612-630`). The document's `resource` is `public_url` with no path (`lib.rs:616-620`). `authorization_servers` is the verifier's issuer (`lib.rs:613-618`). `scopes_supported` is `sign_in_scopes(auth.audiences)` split on spaces (`lib.rs:624-628`).

3. `sign_in_scopes` is fixed text plus one audience scope per configured project id (`auth.rs:41-47`):

   `openid profile offline_access urn:zitadel:iam:user:resourceowner urn:zitadel:iam:org:project:id:<audience>:aud`

   There is no `email` scope. The hub does not discover Nomen's authorization-server metadata in this mode. Discovery (`/.well-known/openid-configuration` and JWKS) runs only for `auth.mode: oidc` and for `trusted_issuers` (`auth.rs:100-102`, `auth.rs:123-125`, `auth.rs:265-307`). Nomen mode never fetches JWKS.

4. Claude runs authorization-code + PKCE against the issuer named in the metadata. For the public client `connect-nomen` creates, the registered redirects are `https://claude.ai/api/mcp/auth_callback` and `https://claude.com/api/mcp/auth_callback` (`crates/hub/src/connect.rs:31-37`, `connect.rs:166-184`). The app is created as a native public client (`OIDC_AUTH_METHOD_TYPE_NONE`) with **opaque** access tokens (`OIDC_TOKEN_TYPE_BEARER`, `connect.rs:172-179`). The browser app is the same token type (`connect.rs:151-158`). The hub does not implement Dynamic Client Registration. `docs/security-baseline.md:17` lists Nomen DCR as planned, not built. The supported Claude path is the pre-registered client id on `/docs`.

5. Claude calls `/mcp` with `Authorization: Bearer <token>`. `Verifier::Nomen` introspects it (`auth.rs:151`, `nomen.rs:358-414`, `nomen.rs:478-525`). The hub authenticates to `POST {issuer}/oauth/v2/introspect` with a private-key JWT from `auth.key_file` (`nomen.rs:80-89`, `nomen.rs:201-209`, `nomen.rs:528-554`). Nomen is not contacted at process start (`nomen.rs:201`, module comment at `nomen.rs:30-31`).

6. `assess` accepts the token only when Nomen says `active`, `iss` is the configured issuer, `aud` contains one of `auth.audiences`, `client_id` is one of `auth.clients`, `exp` is unexpired, and `sub` passes `valid_subject` (`nomen.rs:616-668`, `config.rs:484-501`). The `Subject` handed to MCP is that `sub`, plus `username` as `name` and `urn:zitadel:iam:user:resourceowner:id` as `org` (`auth.rs:21-36`, `nomen.rs:91-110`, `nomen.rs:658-668`). Email is not parsed.

7. MCP tools read that `Subject` from request extensions (`hub.rs:190-205`). `whoami` returns it (`hub.rs:329-366`). A call to a broker is allowed when `subject.id` equals `owner` or a `grants[].subject` (`hub.rs:207-236`, `config.rs:419-422`, `config.rs:465-476`). Otherwise the code is `not_granted`. A machine that account enrolled is the exception: `devices.owner` matches `subject.id` (`hub.rs:226-230`).

8. Every routed call is one row on the hash-chained audit log (`hub.rs:251-317`, `audit.rs:40-51`). `/healthz` reports broker reachability, the audit chain, and `issuer` (`lib.rs:650-688`). `ok` is brokers and the chain only. An identity-provider failure is reported and does not flip `ok` (`lib.rs:670-676`, comment at `lib.rs:670-671`). `issuer.reachable` starts `true` because no call has failed yet (`nomen.rs:168-177`, `nomen.rs:347-355`). It becomes `false`, with `last_error`, only after an introspection error classified as an outage.

Config shape that produces this, required today (`config.rs:124-140`, `config.rs:211-242`):

```json
{
  "listen": "127.0.0.1:8787",
  "public_url": "https://app.devagotchi.com",
  "auth": {
    "mode": "nomen",
    "issuer": "https://id.shippin.cloud",
    "audiences": ["<nomen project id>"],
    "clients": ["<client id>", "<client id>"],
    "key_file": "/absolute/path/nomen-key.json"
  },
  "owner": "<nomen sub of the hub owner>"
}
```

`audiences`, `clients`, `key_file`, and `owner` are mandatory in Nomen mode. `web.client_id` must be one of `auth.clients` (`config.rs:305-314`). The installer copies an operator-supplied `auth` object into `~/.config/devagotchi/hub.json` and installs the key as that account's `idp-key.json` (`deploy/linux/install-devagotchi.sh:363-392`). `connect-nomen` rewrites `auth.audiences` to the `--project` id, appends client ids, and writes `nomen-key.json` under the state dir (`connect.rs:187-214`). Both paths store numeric ids. Nothing re-reads them from Nomen after that.

## Why the live connector fails

Verified against the running hub on 31 October 2026, traced through this tree. The deleted project id is not in the source.

| Live fact | Where it comes from |
|---|---|
| `/.well-known/oauth-protected-resource` advertises `urn:zitadel:iam:org:project:id:397051786973675523:aud` | `scopes_supported` and the `401` scope are `sign_in_scopes` of `auth.audiences` (`auth.rs:41-47`, `lib.rs:495-498`, `lib.rs:624-628`). `397051786973675523` occurs nowhere in the repository. The only input is the deployed `hub.json`. |
| Devagotchi's project is now `396558202386251780` | Not production config. The only occurrence is a fixture audience in `crates/hub/src/connect/tests.rs:228`. |
| Public client `devagotchi-ai-connectors` is `396558926876835844` | Hardcoded on `/docs` (`web/public.rs:39`). The verifier does not read that page. Acceptance is `auth.clients` (`nomen.rs:632-637`). |
| `/healthz` `issuer.last_error` is `Nomen refused the hub's own credentials (400 Bad Request): check auth.key_file and that the hub's API application is active` | That sentence is built only in `NomenVerifier::introspect` when Nomen answers 400, 401, or 403 (`nomen.rs:494-497`). It is stored as an outage (`nomen.rs:450-461`) and copied to `issuer.last_error` while the outage lasts (`nomen.rs:347-354`). `/healthz` prints `issuer` as that struct (`lib.rs:676`). |

What that combination does to Claude:

- The metadata tells Claude to request an audience scope for a project Nomen no longer has. Authorize fails at `id.shippin.cloud` before a token exists. Claude shows "Couldn't connect to the server".
- If a token is presented anyway, the hub does not check it locally. It introspects with the API application named by `auth.key_file` (`nomen.rs:478-488`). A key minted for an application in the deleted project gets `400`. The hub treats that as "Nomen is down" (`nomen.rs:494-497`), answers `503` with `Retry-After: 5` (`lib.rs:528-536`), and will not admit a new bearer until that call succeeds. Confirmed sign-ins from before the failure can still be honoured (`nomen.rs:417-426`). A connector that never completed sign-in has nothing to honour.
- `issuer.reachable` is not a startup probe. It stays `true` until the first failed introspection (`nomen.rs:168-177`). The error text appears only after Claude, or anything else, has already been refused.

`connect-nomen` already knows how to find apps by name and how to write a fresh key (`connect.rs:107-185`, `connect.rs:343-370`). It was not re-run against project `396558202386251780` on the deployed config, or the hub was not restarted onto the file it wrote (`connect.rs:227` tells the operator to `systemctl --user restart devagotchi-hub`). Re-running it would paper over this incident and leave the same break for the next time Nomen restores a project: ids in `hub.json` are a snapshot.

Two further mismatches are in the code even after the ids are corrected, and either one is enough for a strict client to refuse the server:

- Claude's connector URL is `https://app.devagotchi.com/mcp` (`web/public.rs:38`). RFC 9728 path insertion fetches `/.well-known/oauth-protected-resource/mcp` and requires `resource` to be that MCP URL. This handler serves the path (`lib.rs:473-475`) but sets `resource` to `public_url` (`lib.rs:617`) and the `401` points at the root well-known URI (`lib.rs:501-503`). The ChatGPT publication receipt already uses the path form (`verification/website-publication.json` records `resource_metadata` ending in `/oauth-protected-resource/mcp`).
- The AI app's access tokens are opaque (`connect.rs:179`). The security baseline's "JWT against JWKS" row (`docs/security-baseline.md:16`) describes `OidcVerifier` (`auth.rs:311-351`), which Nomen mode does not use. The baseline's "opaque tokens are introspected" row is still marked planned (`docs/security-baseline.md:21`) even though `nomen.rs` is that implementation. Nomen mode cannot admit a user when its own introspection credential is dead, which is the outage on `/healthz` today.

## Users, accounts, orgs, workspaces

There is no user table, no email on the hub's `Subject`, and no provisioning step.

Identity is the token. `Subject` is `id` (`sub`), `via`, `name` (`username`), `org` (resource owner) (`auth.rs:21-36`). `valid_subject` rejects emails and display names in `owner` and `grants` because those strings are not stable (`config.rs:484-501`, tests at `config.rs:785-794`). The CLI stores `email` from userinfo in the machine's `account.json` (`account.rs:69-85`, `account.rs:406-428`). That file is not read by the hub (`account.rs:8-9`). `UserInfo` has no `email_verified` (`account.rs:234-241`).

Stores under `--state-dir`, all SQLite, none of them a user directory:

| File | What it keeps | Identity columns |
|---|---|---|
| `audit.sqlite3` | Hash-chained tool calls (`audit.rs:40-51`) | `subject`, `via` |
| `signins.sqlite3` | SHA-256 of a confirmed token, for an outage (`nomen/kept.rs:42-49`) | `subject`, `name`, `org` |
| `sessions.sqlite3` | Sealed browser tokens (`web/sessions.rs:95-102`) | none; the token is re-verified (`web/routes.rs:103-120`) |
| `devices.sqlite3` | Machines, enroll codes, push subscriptions (`devices.rs:172-194`) | `owner` on devices and codes (migration at `devices.rs:193-194`) |
| `account-boards/<owner>/<project>.sqlite3` | Boards created by `board_bind` (`board/mcp.rs:134-138`, `board/mcp.rs:202-208`) | directory name is `org-<org>` or the account id (`board/mcp.rs:161-168`) |

`hub.json` is the authorization database: `owner`, `grants` (`config.rs:26-36`, `config.rs:166-178`), `brokers`, `boards`. A new Nomen user with a valid token is authenticated. They are not granted any configured broker or shared board (`config.rs:465-476`). They can enroll their own machines, up to 16 (`devices.rs` owner column; route at `hub.rs:226-230`). They get an account board only when something calls `board_bind` (`board/mcp.rs:202-213`). The free limit defaults to one project (`config.rs:64-77`). Nothing creates a board, a grant, or a workspace on first request.

"Org" is Nomen's resource-owner id, copied off the token when the scope asked for it (`nomen.rs:107-110`). It selects the board directory. It is not a Devagotchi organization record. "Workspace" in this repo is the broker's execution root (`crates/core`), not a hub object. The three names `homelab-infra-dev`, `wi-infra-dev`, and `tx-infra-prod` are not inventory. `homelab-infra-dev` and `wi-infra-dev` do not occur. `tx-infra-prod` is a broker name in the `connect-nomen` test fixture (`connect/tests.rs:231-232`). Texas is an operator site in `deploy/linux/README.md:4` and in plan packet 23c (`docs/product-and-platform-plan.md:352`). LXC 110 is not in the tree.

Gaps against "look up by Nomen sub, else link a verified email, else create the user and their own org":

- No row to look up, and no email to link. After a Nomen restore, the hub cannot reconnect a person to an old `sub` by email, because it never stored one. Grants, device owners, and board directories that still hold the old `sub` keep working only if that `sub` is unchanged.
- Unverified email cannot be refused, because it is never read.
- Auto-creating "their own" place is partly already the model (owned machines, `board_bind`, org directory) and is not run on first sign-in. Auto-binding a board would consume the free slot of one (`config.rs:77`, `board/mcp.rs:229` and the limit check below it).
- There is no race to be safe about, because there is no insert.

The insertion points, if a bridge is added, are the two doors that already turn a bearer into a `Subject`: `require_bearer` after `verify` (`lib.rs:519-522`) and `web/routes.rs:who` (`web/routes.rs:109-110`). `NomenVerifier::verify` / `assess` (`nomen.rs:358`, `nomen.rs:616`) is where claims are chosen. `Hub::subject` (`hub.rs:190`) is too late to invent an id without changing every grant comparison. `connect::run` (`connect.rs:53`) remains the operator bootstrap that sets `owner`; it is not the per-user path.

## Proposed design

Fit the existing owners. No new crate, process, or job system. New SQLite lives in the state dir beside `signins.sqlite3`. Network stays inside `hub` (`deny.toml` already allows `reqwest` only there). `Subject.id` stays the account id grants and boards already use, so a bridge cannot silently renumber the owner.

### Identity bridge

New store `identities.sqlite3`, opened like `Audit::open` and `Devices::open` (WAL, `synchronous = FULL`, `busy_timeout`, immediate transaction). Tables:

- `users(user_id TEXT PRIMARY KEY, email TEXT, org TEXT, name TEXT, created_at INTEGER NOT NULL)`
- partial unique index on `email` where `email IS NOT NULL`
- `subjects(issuer TEXT NOT NULL, sub TEXT NOT NULL, user_id TEXT NOT NULL, linked_at INTEGER NOT NULL, PRIMARY KEY (issuer, sub))`

`user_id` for a person who already exists in grants, `owner`, device owners, or board directories **is their current Nomen `sub`**. The bridge does not mint a second id. A later token with a new `sub` and the same verified email attaches a row in `subjects` and the hub keeps presenting the original `user_id` as `Subject.id`. That is what makes "link to an existing account" true for boards and grants. A brand-new person gets `user_id = sub`.

On every authenticated MCP or web request, after the token is valid and before the handler runs:

1. Read `sub`, `email`, `email_verified`, `org`, `name` from the token (JWT claims or the introspection body; both must grow those fields).
2. Look up `(issuer, sub)`. Hit: use that `user_id`. Refresh `name` and `org` on the user row. Do not change `email` unless the new token is verified and the address differs; an email change is an audit row, not a silent merge.
3. Miss, and `email_verified` is true, and `email` matches one user: insert the `subjects` row. Audit `identity.link` with the new `sub` and the existing `user_id`.
4. Miss otherwise (no email, unverified email, or no user with that email): insert `users` with `user_id = sub` and insert `subjects`. Audit `identity.provision`. Do not write a grant. Do not call `board_bind`. Do not enroll a machine. Their "own" place is the one the hub already has: machines they add, and boards they bind, under `Boards::owner_of` (`board/mcp.rs:164-168`). The org claim is stored on the user so the board directory is the org's when Nomen sent one.
5. Two concurrent first requests: `INSERT` … `ON CONFLICT DO NOTHING`, then `SELECT`, inside one immediate transaction. Same shape as `nomen/kept.rs:105-107` and `audit.rs:61-65`. The unique keys make the loser see the winner's row. No in-memory lock is the source of truth.

Unverified email never selects a user and is not stored in `users.email`. Two different verified people cannot share an address: the unique index links the second `sub` to the first user. That is the trade the request asks for. It is wrong if Nomen lets an address move between people without this hub noticing; the audit row is the record, and unlinking stays an operator action on the table, not a tool.

`Subject` gains no new id. `via` stays `nomen` or `oidc`. Audit of tool calls keeps using `Subject.id` (`hub.rs:261-266`). Link and provision are their own audit rows (`tool = identity.link` or `identity.provision`, `broker` empty, `operation_id` the new `sub`) so they sit on the chain `/healthz` already verifies.

Dev tokens (`auth.mode: dev`) do not go through the bridge. They are tests (`config.rs:244-256`).

### Self-configuring Nomen auth

Resolution belongs to `NomenVerifier`, next to introspection, using the management calls `connect.rs` already makes.

Config, with the current fields kept as overrides:

```json
"auth": {
  "mode": "nomen",
  "issuer": "https://id.shippin.cloud",
  "project_name": "Devagotchi",
  "key_file": "/var/lib/devagotchi/.config/devagotchi/idp-key.json"
}
```

`project_name` defaults to `Devagotchi` (`connect.rs:4-8` already assumes that project name). App names default to the constants already in `connect.rs`: `devagotchi-hub`, `devagotchi-hub-web`, `devagotchi-ai-connectors`, plus the CLI client id `devagotchi-cli@shippin` (`account.rs:48`) when that app exists. `audiences` and `clients` become optional. When present they are the accept list, not a hint. `owner` stays required (`config.rs:237-241`).

At start, and on a refresh (proposed: every 10 minutes, and immediately when a JWT's `aud` matches nothing currently accepted):

1. Discover `{issuer}/.well-known/openid-configuration` and fetch JWKS, the same checks as `OidcVerifier::discover` (`auth.rs:265-307`): issuer string must match, `jwks_uri` must be https except loopback tests, empty key set is failure. Cache keys. Unknown `kid` refreshes at most once a minute (`auth.rs:11-13`, `auth.rs:354-360`).
2. If `key_file` loads, sign a client assertion (already `nomen.rs:544-554`) and call Nomen's token endpoint, then `POST /management/v1/projects/_search` with the same `nameQuery` / `TEXT_QUERY_METHOD_EQUALS` body `find_app` uses (`connect.rs:343-348`). Then `apps/_search` per app name, which `Management::find_app` already parses (`connect.rs:343-370`).
3. Write the result to `state_dir/nomen-resolved.json`: project id, client ids, fetched-at. No key material. Mode 0600. The next start uses it if Nomen cannot be asked.
4. If `auth.audiences` or `auth.clients` is non-empty, those ids are what tokens must match. When the search result does not contain one of them, log one line naming the missing id and set `auth.overrides_missing` on `/healthz`. Do not delete the override and do not pretend the token check succeeded. The deleted project `397051786973675523` would show up here instead of being advertised forever.
5. Advertised scopes and the `401` scope use the resolved project id when resolution succeeded, and the override ids only when resolution has not succeeded and the override is all the hub has. Never advertise an id that the last successful search proved absent.

The hub's API application is not granted a manager role by `connect-nomen` (`connect.rs:112-119` creates it and stops). Project search will return 403 until that application can read its own org's projects. The plan's operator step, done once in Nomen and not by this repo, is to give `devagotchi-hub` the role that may search projects and apps. `connect-nomen` should request that role when it creates or reuses the app, so the next join does not depend on a console click. Until the role exists, overrides and the on-disk snapshot still admit JWT users; `/healthz` says the search failed.

If the key is rejected, JWT checks keep using the last snapshot or the overrides. Opaque checks fail for new tokens and `/healthz` says the credential is invalid. That split is the point of the next section.

### Token validation

In `NomenVerifier::verify`, before introspection:

- Three dot-separated segments: JWT. Verify with the cached JWKS. Require `iss` (trailing slash ignored, as today), `exp` with the existing 30 s leeway (`nomen.rs:77`), `sub` via `valid_subject`, and `aud` containing the resolved project id or an accepted client id. Require `azp` or `client_id` to be an accepted client, the same rule `assess` applies to opaque tokens (`nomen.rs:632-637`). Read `email`, `email_verified`, and the resource-owner claim. Do not introspect. A dead `key_file` does not matter.
- Anything else: opaque. Existing introspection (`nomen.rs:478`). A 400/401/403 from Nomen sets `credential_error` and does not, by itself, mark every JWT as unusable. New opaque tokens still get `503`. Previously confirmed opaque tokens follow the outage rules already in `nomen.rs:417-426`.

This does not help the live Claude connector until the public app issues JWTs. `connect-nomen` creates it with `OIDC_TOKEN_TYPE_BEARER` (`connect.rs:179`). Zitadel will keep issuing opaque tokens until that app's access-token type is JWT. The code change and a Nomen setting are both required. Existing opaque tokens keep working through introspection while the key is valid. `connect-nomen`, when it finds the app already there, should report the token type and not silently leave it opaque.

Add `email` to `sign_in_scopes` (`auth.rs:41-47`) so the bridge can see a verified address. Web sign-in uses the same function (`web.rs:146`).

### MCP metadata for claude.ai

Keep both well-known routes. Make them differ:

| Request | `resource` | `WWW-Authenticate` `resource_metadata` |
|---|---|---|
| `POST /mcp` and `GET /.well-known/oauth-protected-resource/mcp` | `{public_url}/mcp` | `{origin}/.well-known/oauth-protected-resource/mcp` |
| `GET /.well-known/oauth-protected-resource` | `{public_url}` with no path | the root well-known URI |

`authorization_servers` stays the issuer (`lib.rs:618`). `bearer_methods_supported` stays `header`. `scopes_supported` comes from the resolved project, including `email`. Do not add `registration_endpoint`. The hub is not an authorization server and Nomen's DCR is still the planned item in `docs/security-baseline.md:17`. Claude's working path remains "use your own OAuth client" with the pre-registered id (`web/public.rs:36-40`). `/docs` should print the resolved `devagotchi-ai-connectors` client id from the snapshot, not the constant `396558926876835844`, which is already a second copy of an id that can rot.

The resource parameter Claude sends back to Nomen (RFC 8707) is Nomen's concern. The hub's job is to make the metadata's `resource` equal the URL Claude was given.

### `/healthz`

Leave `ok` as brokers plus the audit chain (`lib.rs:668-686`). A bad Nomen key must not take linked machines out of service; that is why issuer state is reported and not counted (`lib.rs:670-671`). Add an `auth` object a deploy smoke can require:

- `issuer_reachable` — discovery document fetched, issuer matches
- `jwks_age_seconds` and `jwks_fresh` — last good JWKS, fresh if younger than the refresh interval
- `project` — `{ "name", "id", "found" }`
- `clients` — name, id, found, for each app name
- `credential` — `{ "valid", "error" }` with the introspection or token-endpoint error, including the sentence already built at `nomen.rs:496`, or null when the key has not been used
- `overrides_missing` — configured ids the last search did not find
- `ready` — issuer matches, JWKS is fresh, project was found (or a still-present override is in use), and at least one accepted client was found

`ready: false` stays HTTP 200. Readiness for a deploy is the JSON field. Liveness for Caddy stays `ok`.

### Alternatives considered

| Alternative | Why it is not the proposal |
|---|---|
| Hand-edit `hub.json` and re-run `connect-nomen` only | Fixes this incident. The next project restore repeats it. The owner asked for the hub to survive a Nomen change without a hand-written id. |
| Accept any token the issuer signed, ignore `aud` | Lets every Shippin application's user into this hub. `assess` exists to prevent that (`nomen.rs:10-13`, `nomen.rs:625-637`). |
| New internal user id for everyone, grants rewritten to it | Breaks `owner`, every grant, every device owner, and every board directory in one restart. `valid_subject` and `whoami` document that the id is the provider's `sub` (`hub.rs:98-99`, `config.rs:484-486`). |
| Link on unverified email, or on `username` | The same address or login name has already belonged to two accounts (`config.rs:484-486`). The request forbids the unverified link. |
| Auto-`board_bind` a default project, or auto-grant `tx-infra-prod` | Spends the free board (`config.rs:77`) and would hand Jesse's machines to the first person who signs in with Claude. Owned machines and an explicit grant stay the way in. |
| Implement RFC 7591 in the hub | The authorization server is Nomen. The baseline already parks DCR there (`docs/security-baseline.md:17`). A second registrar splits the client list the verifier checks. |
| Introspect every token, including JWTs | Keeps every sign-in behind `auth.key_file`. That is the current outage. |
| Drop introspection | Opaque tokens from the app as `connect-nomen` creates it (`connect.rs:179`) would all fail. Keep introspection for those. |
| A new service that holds users | The plan's boundary test says extend the hub. The state dir already holds the other per-hub stores. |

## Build, test, CI, deploy

### What runs today

| Check | Where |
|---|---|
| `cargo build --locked --bins`, `cargo nextest run --locked` on Linux x86_64 and arm64, Windows 2022, Windows 2025, Windows 11 arm, macOS | `.github/workflows/ci.yml:16-45` |
| `python tests/e2e.py`, which includes `tests/nomen_probe.py` and `tests/web_probe.py` | `ci.yml:47-66`; probe calls at `tests/e2e.py` (nomen and web after the dev-token hub) |
| Windows installers, Linux `shellcheck` plus `deploy/linux/ci-install-check.sh` (systemd hub behind Caddy, Nomen-mode metadata, `/healthz`) | `ci.yml:71-98` |
| `cargo deny check` on every PR and weekly | `.github/workflows/security.yml` |
| cargo-dist: every `main` commit is built and kept as a workflow artifact; a version tag publishes a GitHub Release | `.github/workflows/release.yml:41-49` |
| Fresh-runner install of those release artifacts | `.github/workflows/install-check.yml`, called from the release workflow |

No clippy or rustfmt job. No container image. Packet 16 is still the unbuilt GHCR image (`docs/product-and-platform-plan.md:334`, `product-and-platform-plan.md:348`). Nothing in `.github/` deploys.

Hub tests that already stand in for Nomen:

- `crates/hub/src/nomen/tests.rs` — introspection, audience, client, expiry, outage, the hub key being refused (`nomen_refusing_the_hubs_own_key_is_an_outage_not_a_sign_out`), key file mode.
- `crates/hub/src/auth.rs` — `oidc_verifies_a_real_signed_token`, federated ES256, dev tokens. JWKS is the OIDC path only.
- `crates/hub/src/connect/tests.rs` — fake management API, app create, key written 0600, `tx-infra-prod` preserved.
- `tests/fake_nomen.py` — introspection that checks the hub's client assertion, browser PKCE token endpoint, device flow, userinfo. No JWKS, no project search.
- `tests/nomen_probe.py:179-181` — metadata names the issuer. `tests/nomen_probe.py:220-222` — `/healthz` shows an outage. `tests/hub_probe.py:102` — `401` contains `resource_metadata=`.
- `verification/hub-h1.json` — dev-token MCP against a broker, plus discovery and JWKS rejection against the real issuer. No minted user token. Recorded 21 October 2026.

### Deploy as the repo actually describes it

`sudo ./install-devagotchi.sh --role hub --version <semver> --public-url https://… --auth-config FILE --caddy` (`deploy/linux/README.md:55-57`, `install-devagotchi.sh:14-15`). The unit is the hub user's systemd user service (`install-devagotchi.sh:433-451`):

`ExecStart=/opt/devagotchi/current/devagotchi hub serve --state-dir %h/.local/state/devagotchi-hub --config %h/.config/devagotchi/hub.json`

Rollback of the binary is re-running the installer with the previous `--version` (`deploy/linux/README.md:26`). Versions sit under `/opt/devagotchi/versions/<version>/`. There is no staging host, no smoke job, and no script that knows Proxmox or LXC 110.

`scripts/lay-down-dist.sh` fills the hub's `downloads` directory from a GitHub build of a `main` commit so `/join` can hand that build to machines (`CHANGELOG.md:115-116`). That is the distribution path for brokers, not a hub rollout.

### Gaps against a pipeline that promotes only a main build

- No job fails a PR on rustfmt or clippy.
- Fake Nomen does not serve JWKS or `projects/_search`. The new tests have to extend `tests/fake_nomen.py` or, better for unit tests, stay inside `nomen/tests.rs` the way introspection tests already embed an HTTP fake.
- Artifacts are cargo-dist archives tied to a tag or a `main` workflow run, not an image tag. A second unpublished image would fork packet 16. Prefer the dist archive, checked by SHA and GitHub attestation, which `scripts/lay-down-dist.sh` already requires for machine builds.
- "Only main is deployable" is not enforced because nothing deploys. The rule belongs in the deploy script: refuse a SHA that `git merge-base --is-ancestor` says is not on `origin/main`, and refuse a dist artifact whose attestation is not that SHA.
- Staging, the smoke's test client, and LXC 110 are not in the repo. A script that SSHs somewhere it cannot name would be fiction. Packet 23c says deploying the hub on Texas is an approved step through Devagotchi, not an SSH session (`docs/product-and-platform-plan.md:352`). Those two stories disagree; see the questions.

## Work plan

One concern per PR is the repo rule (`CLAUDE.md`). The owner asked for identity, auth, CI, and deploy together. Recommended split: this plan; then the hub behaviour; then the pipeline, after the questions below are answered. Doing the pipeline in the same change as the verifier mixes a production rollout mechanism into an auth patch and cannot be reviewed against a host the repo does not describe.

### Phase A — hub behaviour (the implementation PR)

Files:

| File | Change |
|---|---|
| `crates/hub/src/config.rs` | Optional `project_name` (default `Devagotchi`), optional `audiences` and `clients`. Keep `key_file` required until the open question on a keyless hub is answered. Validation: overrides that are present must still be non-empty strings; `owner` stays. Tests that build a Nomen config (`config.rs` `nomen()` helper) keep passing by still setting audiences and clients. |
| `crates/hub/src/nomen.rs` | JWKS cache; JWT branch in `verify`; introspection only for opaque tokens; credential failure distinct from "Nomen unreachable"; `email` / `email_verified` on the introspection struct; resolved accept-list behind a lock. |
| `crates/hub/src/nomen/resolve.rs` (new, same module) | Project and app search, snapshot file, refresh task. Parse with the same JSON shapes as `connect.rs:343-370`. |
| `crates/hub/src/auth.rs` | `sign_in_scopes` adds `email` and takes the resolved project id. `Subject` unchanged except the bridge may replace `id` with the linked `user_id` when they differ. `issuer_health` grows the `auth` fields, or healthz reads them from the verifier directly. |
| `crates/hub/src/identity.rs` (new) | The store and `resolve_user`. Called from `require_bearer` and `web/routes.rs:who`. |
| `crates/hub/src/lib.rs` | Path-correct metadata and `WWW-Authenticate`. `/healthz` `auth` object. Spawn the refresh task next to the board reaper (`lib.rs:382-386`). |
| `crates/hub/src/connect.rs` | When creating the AI and web apps, request JWT access tokens, or record that this is a follow-up if changing token type for an existing app is unsafe (question below). Ask for the manager role on `devagotchi-hub`. |
| `crates/hub/src/web/public.rs` | Stop hardcoding `396558926876835844`. The page is static today; it needs the resolved id from `AppState`, so `docs` can no longer be a free function with no state. |
| `crates/hub/src/nomen/tests.rs`, `auth.rs` tests, new `identity` tests | Cases in the next section. |
| `tests/fake_nomen.py`, `tests/nomen_probe.py` | JWKS document, a signed JWT accepted without a valid hub key, metadata `resource` for the `/mcp` well-known, `/healthz` `auth.ready`. |
| `docs/auth.md` | Short operator page: bridge, minimal `hub.json`, what `/healthz` `auth` means, Claude uses the pre-registered client, DCR is not offered. |
| `docs/security-baseline.md` | Correct the two drifted rows (`:16` and `:21`) in the same PR so the baseline matches the code. |

Tests to land with phase A, all against a fake issuer, no call to `id.shippin.cloud`:

- New `sub` provisions a user whose id is that `sub`; second call returns the same row; audit has one provision.
- Known `sub` returns the existing user and does not insert another.
- Unknown `sub` with `email_verified: true` and a stored email links, and `Subject.id` stays the old user id.
- `email_verified: false` or missing does not link, even when the address matches; a new user is provisioned with no email stored.
- Two concurrent first requests for the same `sub`, and two for the same verified email, end as one user (barrier test around the transaction).
- Resolver: fake `/management/v1/projects/_search` and `apps/_search` return the Devagotchi project and `devagotchi-ai-connectors`; scopes in the metadata use that project id.
- Override id absent from the fake search is still accepted for tokens and appears in `overrides_missing` and a log line.
- JWT signed by the fake JWKS, `aud` containing the project, is accepted when introspection is programmed to return 400.
- Opaque token still introspects; with a 400 from Nomen, that call is `503` and `auth.credential.valid` is false while `auth.ready` can still be true if the project snapshot and JWKS are good.
- `/.well-known/oauth-protected-resource/mcp` has `resource` ending in `/mcp`; the root document does not; `POST /mcp` without a bearer advertises the path metadata URL.

`cargo nextest` for `devagotchi-hub` and `python tests/nomen_probe.py` are the bars. Full `tests/e2e.py` before the PR merges, per `CLAUDE.md`.

### Phase B — pipeline (separate PR, after the questions)

- Do not invent a container if packet 16 is still the owner of the image. The deployable artifact is the cargo-dist archive for a SHA that is on `main`, attested, fetched the way `scripts/lay-down-dist.sh` fetches machine builds.
- A script under `deploy/linux/` that: takes a SHA and a host description from outside the repo (env or a file that is not committed with secrets); refuses a SHA not on `main`; installs that version next to the current one with the existing installer; runs smoke against a staging base URL; on success flips production the same way; on failure runs the installer with the previous version (`deploy/linux/README.md:26`).
- Smoke: `GET /healthz` and require `auth.ready`, `GET` both well-known documents and require the `/mcp` resource, one PKCE authorization-code login against Nomen with a test client, then MCP `initialize` and `tools/list` with the resulting bearer. The PKCE login needs a test user that already exists. This plan does not create Nomen accounts.
- `docs/deploy.md` written in that PR, not this one, so it cannot document a host it does not know.

### Risks

- Linking by verified email merges two Nomen accounts into one Devagotchi user. Grants of the first `sub` apply to the second. That is the request. It is also an account takeover if Nomen marks an address verified for the wrong person. Mitigation: exact address match, `email_verified` required, audit row, no automatic unlink.
- Changing `Subject.id` for a linked user changes which grant matches. Only the email-link path does that, and only toward the older id. A person whose `sub` survived the restore is unchanged.
- JWT and opaque acceptance can disagree if `assess` and the JWT check drift. One function should decide "this `aud` and this client are allowed", called from both.
- Refreshing the accept-list must not take the process down. A failed refresh keeps the snapshot. A successful refresh that drops the owner's client would lock the owner out if `owner` is enforced only as a `sub` and the owner's token client disappeared. Do not remove an override that is still in the file; do not drop the last accepted client on an empty search result.
- `sign_in_scopes` adding `email` changes what the browser login asks for (`web.rs:146`). Nomen must already allow that scope on `devagotchi-hub-web`. It is a standard OIDC scope; confirm on the real project before relying on the claim.
- Path-specific `resource` can break a client that was written against the current document, where `resource` is the origin. The root document stays the origin so that client still has a URL. Claude, given `…/mcp`, should follow the path document.
- Classifying a 400 as "credential" instead of "outage" changes `nomen_refusing_the_hubs_own_key_is_an_outage_not_a_sign_out`. Opaque tokens should still be honoured through that failure when they were confirmed earlier. The test needs to be updated, not deleted.
- `/docs` becoming dynamic is a behaviour change for a page directories link to. It must stay free of secrets and free of script.

### Open questions for Jesse

1. Confirm the account-merge rule: a new Nomen `sub` with the same verified email becomes the old Devagotchi user and inherits that user's grants and machines. If that is too strong, the bridge can record the link and still require an operator to flip grants.
2. Nomen restore: did user ids (`sub`) survive, or only the project was recreated? This hub has no stored emails, so it cannot repair grants whose `sub` changed. If subs changed, list the ones that must be linked by hand once, before the automatic path has any email to match.
3. May `devagotchi-hub`'s API application call `POST /management/v1/projects/_search` and `apps/_search`? `connect-nomen` never grants it a role. Who applies that role on `id.shippin.cloud`?
4. Switch `devagotchi-ai-connectors` (and the web app) from `OIDC_TOKEN_TYPE_BEARER` to JWT access tokens? Until that happens, Claude's tokens stay opaque and a dead hub key still blocks them. Existing sessions would keep working only through introspection.
5. Should a hub with no `key_file` start, JWT-only, when a snapshot or an override already names the project? The proposal keeps `key_file` required so opaque tokens and refresh still have a path.
6. Is `auth.ready == false` allowed to stay HTTP 200? The proposal says yes, so Caddy does not withdraw the hub while machines are linked. Say if the edge should instead fail the health check.
7. Where is staging, and what is the smoke's existing Nomen test user and test client? The repo has neither. A full PKCE smoke against production Nomen is a real login; it must not create an account.
8. How is Texas reached for a hub rollout: SSH to LXC 110, `pct exec 110`, or packet 23c's approved Devagotchi step with no SSH? The script cannot be written until this is one of those.
9. Are `homelab-infra-dev`, `wi-infra-dev`, and `tx-infra-prod` broker names that belong in the hosted `hub.json` `brokers` list, or only machines the owner enrolls? They are not in the tree. Auto-provision must not grant them. Confirm that a new Claude user gets no route to those machines.
10. Split phase A and phase B, or override the one-concern rule and ship the pipeline in the auth PR? Recommendation is two PRs, and no deploy from either until you run it.
