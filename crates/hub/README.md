# devagotchi hub

The bastion (plan §11a, N13). Every AI reaches a machine through it: the hub proves who is
calling with the identity provider, checks that subject's grants, and routes the broker's
own four handle-based tools to the broker that owns the target machine. The hub never
admits a job; the broker does, and the job outlives the hub, the connection and the client.

```text
claude.ai / ChatGPT ──mcp.shippin.cloud──┐  (your own Nomen token, passed through)
Claude Code / Codex ──device code────────┤   ┌──────── devagotchi hub ────────┐
your own agents ──service user───────────┴──▶│ bearer → subject → grants → route │──▶ broker (gold-ws)
                                          │ audit: hash-chained SQLite        │──▶ broker (gus)      (M1)
                                          └───────────────────────────────────┘──▶ broker (dabo)     (M1)
```

## Run

```bash
devagotchi-hub --state-dir ~/.local/state/devagotchi-hub --config ~/.config/devagotchi-hub/hub.json
# devagotchi hub owner: 300000000000000001 reaches every broker
# devagotchi hub ready: http://127.0.0.1:8787/mcp (2 brokers, auth nomen https://id.shippin.cloud)
```

```json
{
  "listen": "127.0.0.1:8787",
  "public_url": "https://hub.shippin.cloud",
  "auth": {
    "mode": "nomen",
    "issuer": "https://id.shippin.cloud",
    "audiences": ["<Nomen project id the hub's API application belongs to>"],
    "clients": ["<mcp.shippin.cloud's Nomen client id>", "<shippin-bridge's client id>"],
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
[`devagotchi link`](../link/README.md), which holds one outbound WebSocket to `/link`. No port
opens on the machine. The device proves its enrolled Ed25519 key over a fresh challenge;
the hub then sends one call per routed request and waits up to 30 s for the answer.

```bash
devagotchi hub enroll  --state-dir DIR --device gus   # one-time code, 15 minutes, stdout only; supersedes older ones
devagotchi hub revoke  --state-dir DIR --device gus   # link closes within 2 s; pending codes cancelled
devagotchi hub devices --state-dir DIR                # enrolled machines, JSON
```

### Add a machine with one line

With `"downloads": "/opt/devagotchi/current/dist"` in the config, a directory holding
`VERSION`, `SHA256SUMS` and the files it lists (the GitHub build the hub runs: the archives
for each target and both installers; `scripts/lay-down-dist.sh COMMIT DIR` lays it down),
"Add a machine" on `/account` hands a signed-in person one line per OS:

```bash
curl -fsSL https://app.devagotchi.com/join/dvg-… | sudo sh      # Linux, x86_64 or aarch64
irm https://app.devagotchi.com/join/dvg-….ps1 | iex             # Windows PowerShell, no admin
```

`/join/<code>` answers with a script for that code: the machine's name, the build version and
the SHA-256 of each file it then downloads from `/dist/<file>`. It installs that build, makes
the machine's own key, enrolls it (the code goes to the installer on stdin, or in the
environment on Windows), runs commands as the installed account (`exec_policy: account`) and
starts the broker and the link at boot. Fetching the script does not spend the code; enrolling
does. A spent, expired or unknown code gets a script that only says so. The hub hashes every
listed file at start and leaves `/join` off (reported in `/healthz` `downloads`) when one does
not match. Operator codes from `devagotchi hub enroll` work with `/join` too.

A linked broker that is not connected answers `broker_offline`; one that never enrolled,
`not_enrolled`; a revoked one, `device_revoked`; one with 64 calls already waiting,
`link_busy`. A refused device is told only `refused`; why is in the audit. `/healthz` reports each linked broker as
`connected` without counting an asleep laptop against the hub's own health.

## Authentication

| Mode | Bearer token | Who it is for |
|---|---|---|
| `hub` | None yet (the hub issues its own to AI clients in 32b); with an optional `nomen` block, Nomen tokens as in `nomen` mode. People sign in on the hub's own pages with passkeys | Any hub that should work with nothing else deployed (D25), devagotchi.com included |
| `nomen` | Any Nomen access token, opaque or JWT, checked by asking Nomen (introspection) as the hub's own API application | Shippin: your sign-in at console.shippin.cloud, relayed by mcp.shippin.cloud; `shippin-bridge` sessions |
| `oidc` | A JWT from any OpenID Connect issuer: `iss`, `exp`, `aud`, `sub` required; RS256/384/512 or ES256/384; keys from the issuer's JWKS, refreshed on an unknown `kid` at most once a minute | Self-hosters whose identity provider issues JWT access tokens |
| `dev` | A static token from a 0600 file `{ "<token>": "<subject>" }` | Tests only (plan D21): accepted when `public_url` is loopback http; a hub at an address people reach refuses to start with it |

Unauthenticated requests get `401` with `WWW-Authenticate: Bearer resource_metadata="<public_url>/.well-known/oauth-protected-resource"`;
the metadata names the issuer. A bad token adds `error="invalid_token"`. When the identity
provider cannot be reached the hub answers `503`, never `401`.

## The hub's own accounts (`auth.mode hub`)

```jsonc
"auth": { "mode": "hub", "rp_id": "devagotchi.com" },   // rp_id: optional, the hub's host by default
"owner": "395290028818825218"                           // optional: an earlier provider's subject to keep
```

The hub keeps its accounts in `accounts.sqlite3` (0600) in its state directory and signs
people in itself with passkeys (WebAuthn, verified on `aws-lc-rs`; ES256, EdDSA and RS256;
user verification on every ceremony). Nothing else needs to be running. `rp_id` is the
passkeys' relying party: the hub's host or a parent domain of it, and it can never change
for passkeys already made. Plain `http` works only on loopback, for tests.

The owner is made on the hub's host, by whoever can write its state directory (the same
trust as `hub enroll`):

```bash
devagotchi hub owner --state-dir DIR --config /etc/devagotchi/hub.json [--name Jesse]
# https://app.devagotchi.com/owner#dgl_…   (stdout only, once; 15 minutes)
```

Open the link on the device that will hold the passkey and press **Make a passkey**; that
signs you in. The first run makes the owner's account, with the config's `owner` as its id
when it names one, so a hub that had a Nomen owner keeps every board, machine and audit row
keyed by it. Run it again to add a passkey: using the new link ends the owner's other
sessions. After that, `/auth/login` signs in with the passkey. Sessions last 30 days unused
and 90 at most, end when their passkey is removed or the account is disabled, and are kept
only as hashes.

Challenges are signed by the hub and stored only once used, so unanswered sign-ins cost the
hub nothing; each also rides in a `__Host-` cookie, so another site cannot sign a browser in
to an account of its choosing. The passkey pages run one script, the hub's own, under a
strict Content-Security-Policy.

## Shippin sign-in (Nomen)

Nomen's access tokens are opaque: only Nomen can say whose they are. The hub asks it about
each token (RFC 7662 introspection), signing in as its own API application with a
private-key JWT. Nomen answers `active` only when the token is live and the hub's
application is in its audience; the hub then requires the issuer, an accepted project in
`aud`, an accepted application in `client_id`, an unexpired `exp` and an account ID
(`sub`) itself, taken exactly as Nomen states it.

Any application in Nomen can ask for the hub's audience, and a person already signed in to
Nomen can be handed a token by any of them without typing anything. So the audience only
says a token may be shown to the hub: `auth.clients` names the applications whose
sign-ins count (mcp.shippin.cloud, shippin-bridge), and grants match the account ID
byte for byte.

Set up once in Nomen (a Gold change, through gold-policy):

1. In the project the hub belongs to, add an **API** application with **Private Key JWT**.
2. Add a key and download it. That JSON (`type`, `keyId`, `key`, `clientId`) is `auth.key_file`;
   keep it in Vaultix and write it at deploy as the hub's user, mode 0600. On Linux and
   macOS the hub refuses a file others can read; on Windows, put it under the hub user's
   private directory. Its errors never print the key.
3. Put that project's id in `auth.audiences`. The applications whose tokens should reach
   the hub (mcp.shippin.cloud's, shippin-bridge's) must be in the same project, or request
   the scope `urn:zitadel:iam:org:project:id:<project id>:aud`; list their client ids in
   `auth.clients`.
4. Set `owner` to your account ID. Sign in and call `whoami` once if you are not sure which it is.
   On a server, `install-devagotchi.sh --role hub --auth-config FILE` takes the `auth` object
   (and `owner`) from FILE, installs the key named by `auth.key_file` as the hub account's own
   0600 file and points the config at it; `--person`/`--agent` are then account IDs, let in
   by grant.
5. For sign-in from a terminal (`devagotchi login`, D22), add a **Native** application to the
   same project with **Device Authorization** and **Refresh Token** grants, client id
   `devagotchi-cli@shippin` (or pass `--client-id`), and list it in `auth.clients`: the token
   `login` lands is then one this hub accepts, and `devagotchi mcp --hub` relays an agent's
   stdio MCP session here as that account.

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
hub within a minute and a Nomen blip does not interrupt a session already confirmed. At most
16 questions to Nomen are in flight; a flood of made-up tokens can slow new sign-ins but
never evicts a confirmed one, so rate-limit `/mcp` per client address at the edge (Caddy).
Each refused token logs its reason to stderr, at most one line a second, never the token. Agent
personal access tokens are issued for Nomen's own API, so the hub does not accept them;
agents get their own identity through token exchange in a later increment.

## Browser sign-in: your account and machines

With a `web` section, people sign in with their Shippin account at `<public_url>/account` and
see who they are signed in as (login name and account ID) and the machines they can reach:
each broker they hold a grant for (every broker for the owner), whether it is on the hub's
host or a linked machine, and whether it is online, offline, revoked or not enrolled yet.
`GET /api/v1/me` returns the same view as JSON, for a browser session or a bearer token.

```json
"auth": { "mode": "nomen", "clients": ["<mcp.shippin.cloud's id>", "<devagotchi web app's id>"], … },
"web": { "client_id": "<devagotchi web app's id>" }
```

Set up once in Nomen, in the hub's project: a **User Agent** application with **PKCE** (no
secret) and the **Refresh Token** grant, redirect URI `<public_url>/auth/callback`. Its client id goes in `web.client_id`
and in `auth.clients`; the hub refuses to start otherwise, and with any auth mode but
`nomen`, or a `public_url` that is plain http anywhere but loopback.

### Trusting devagotchi.com sign-ins (`trusted_issuers`)

A person signed in on devagotchi.com reaches their own machines with that site's token:

```json
"trusted_issuers": [
  { "issuer": "https://<project>.supabase.co/auth/v1", "audiences": ["authenticated"] }
]
```

The token goes to the issuer its `iss` names and must be signed by that issuer's JWKS keys
(ES256 for the site); anything else goes to `auth` as before. Grants and ownership key on the
token's `sub` (the site's user id). The site adds machines with `POST /api/v1/machines`
(`{"name": "laptop"}`, bearer only) and lists them with `GET /api/v1/me`.

How it holds up:

- Authorization code with PKCE (S256). The `state` is single-use and bound to the browser
  by a ten-minute cookie, so another browser's callback, a replay or a forged link fails.
- The access token Nomen returns goes through the same introspection as every bearer
  token: accepted client, accepted project, exact account ID. A browser session is never
  weaker than an MCP call, and Nomen down is a 503 page, never "not accepted".
- The browser gets only a random session id, in a `__Host-` cookie (`Secure`, `HttpOnly`,
  `SameSite=Lax`). The hub keeps only its SHA-256, and Nomen's tokens sealed on disk:
  AES-256-GCM under `session.key` (0600) in the state directory, in `sessions.sqlite3`
  (0600). A restart signs nobody out, so a phone that turned on notifications opens a
  push straight onto Approve.
- The hub asks for `offline_access`. With a refresh token, a session lasts up to 90 days
  and ends after 30 days unused. The hub renews the access token through Nomen as it runs
  out, one refresh at a time, keeping the rotated refresh token. If Nomen refuses a
  refresh (account disabled, token revoked), the session ends. Without a refresh token,
  a session ends with its access token or after 12 hours, whichever is first. The Nomen
  app needs the refresh-token grant for the long session.
- Every page re-checks the access token (Nomen's answer is reused for a minute).
- The return path after sign-in is kept on the hub and is only ever a path on it.
- Every page's top bar says where you stand, decided locally from the session store (no
  call to Nomen): signed in, Approvals, Machines and a one-tap **Sign out**; signed out, one
  **Sign in** that comes back to the page you were on. Signed out, `/` says what Devagotchi
  is and offers Sign in with Shippin; signed in, it is your machines.
- Pages carry no script; values are escaped and the CSP is `default-src 'none'` with the
  hub's own stylesheet. Sign-out ends the hub session and revokes its refresh token at Nomen; the Shippin sign-in stays.

## Tools

`machines` is where an AI starts: every machine the subject may use, local or linked, with its
status right now (`online`, `offline`, `not_enrolled`, `revoked`), the same list the account
page and `/api/v1/me` show. `whoami` returns the proven account ID, the login name, whether it
is the owner, and the brokers it may use. `operation_start`,
`operation_get`, `operation_read` and `operation_cancel` take `{ "broker": "<name>", "input": {…} }`
where `input` is exactly the broker's own contract. A subject without a grant for that
broker gets `not_granted`; an unknown name gets `unknown_broker`; the broker's own refusals
(`executable_not_allowed`, …) pass through unchanged.

The `board_*` tools are the agents' board (`docs/agent-board.md` §6): the `boards` the config
names, shared by grant, and — with an `account_boards` section — each org's own, made by
`board_bind` (B13: the org the identity provider proves the account belongs to, Nomen's
`urn:zitadel:iam:user:resourceowner:id`; every member of the org finds the board, another
org never does; an account with no org gets boards of its own). One project per free org or
account, `plan_limit` beyond; the owner and the account or org IDs in
`account_boards.unlimited` have no limit. The sign-in pages, `devagotchi login` and the
protected-resource metadata (`scopes_supported`, and `scope` in the 401 challenge) ask Nomen
for exactly `openid profile offline_access urn:zitadel:iam:user:resourceowner` and the hub's
audience. A caller names its seat on a board with `X-Devagotchi-Seat: <name>`: the account's
own agent, the actor on the account's board and `<account>/<seat>` on an org's or a shared one.

## First run

Connecting never lands on an empty board. When the hub first opens a board that has never
had a ticket, it puts a **Getting started** ticket in the attention state: ask your AI which
machines it can reach, turn on phone notifications and send yourself a test approval, run one
small thing on a machine, then have the AI close the ticket. An AI's `board_prime` shows it
under `attention`; agents never claim it. On `/approvals`, **Send me a test approval** puts a
harmless approval on the person's first board and pushes it to their own phones only, so the
whole loop is checked with one tap and no AI. Approvals read "Your AI asks" when the person's
own account asked, and "Devagotchi asks" for the test.

## Audit and health

Every routed call appends one row to `audit.sqlite3` in the state directory: time, subject,
how it authenticated, tool, broker, operation id, outcome, `prev_hash`, `hash`. A call that
cannot be recorded is refused. `GET /healthz` reports each broker's reachability and walks
the chain (`{"ok": true, "brokers": [...], "audit": {"entries": N, "ok": true, "head": "…"}}`).

## Check

`python3 tests/hub_probe.py http://127.0.0.1:8787 --token <bearer> --broker gold-ws --cwd ~`
walks the surface end to end against a live broker and exits 0 only when every step holds.
`tests/web_probe.py` signs in through a browser's eyes against the real binary and a fake
Nomen (PKCE, introspection, the account page, refusals, an outage and sign-out).

## Not in H1

mcp.shippin.cloud passing your Nomen token through to the hub (D2), approving a new
machine from your phone after Shippin sign-in, grants read from the console's staff roles
and server registry once they are live, agent identities by token exchange, installer
autostart for the link, adding your own machine from the account page (machines are
still granted in this file), the agent-hub board (H3), harness profiles (H4), the bundle (H5).
