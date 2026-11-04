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

Self-hosting with Docker (the hub, its own Postgres and Caddy, one command):
[`deploy/compose/`](../../deploy/compose/README.md). Or the binary directly:

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

## The site

Every word, name, link, colour and picture the hub shows comes from its **site**, not its
code: the landing page and the public pages (`/docs`, `/privacy`, `/terms`, `/support`), the
name in the bar and in every page's title, the sign-in email, the passkey prompt, the
installed app's name and icon, the board's getting-started ticket, and the MCP server's name
and instructions every AI client is given. Design tokens and content are kept apart from the
code that lays them out, so one hub binary serves any brand.

With no site configured the hub serves the built-in one, compiled in from
[`site/default/`](site/default/). To serve your own, point `site.dir` at a directory, or set
`DEVAGOTCHI_SITE_DIR` (it wins over the file):

```json
"site": { "dir": "/etc/devagotchi/site" }
```

The directory holds:

| File | What it holds |
|---|---|
| `site.json` | **Required.** `product` (name, short name, tagline, description, marketing domain), `operator` (the legal name the privacy policy and terms use, contact, support email), `links` (public links by name; `hosted_hub` is the hub `devagotchi login` signs in at by default), `sign_in` (`order`: the order of the sign-in buttons by provider name; `labels`: what each provider is called, `nomen` for Nomen sign-in), `landing.sections` (the landing page's sections in order, each `"on": true` or `false`), `mcp` (`name`, optional `title`, `instructions`), `plans`, `services`, `early_access`, and `assets` (which files are the logo, icon, mascot, diagram, video and poster) |
| `tokens.json` | The design tokens the stylesheet reads as custom properties: `colors` and `dark` (`--bg`, `--accent`, …), `fonts` (`--font-ui`, `--font-display`, `--font-mono`), `radii` (`--radius-s` … `--radius-pill`), `layout` (`--bar-h`, `--gutter`, `--wide`, `--narrow`), `theme_color` and `background_color` for the installed app, and `font_files` to serve fonts from the hub itself |
| `content/landing.json` | Every landing section's copy: the hero, how claiming works, How it works, the comparison and its evidence, features, plans and services, connecting an AI, self-hosting, what is coming, the questions, the closing, the footer, the popups, the glossary, and `custom` sections of your own |
| `content/messages.json` | The sign-in email (`email.subject`, `email.body`, `{link}` is the one-time link), the getting-started ticket (`welcome.title`, `welcome.body`, `{hub}` is the hub's address), the line under every public page, and the words of a workflow's page (`workflow`) |
| `content/workflows.json` | The setups an AI hands its person as one link (`workflow_start`): each an `id`, a `title`, an `intro` and its ordered `steps`, each a `kind` the hub runs (`machine` with an optional `os`: `linux`, `windows`, `docker` or `host`; `phone`; `ai_client` with its `client`: `claude`, `chatgpt`, `grok` or `cursor`; `secure_account`; `done`) with its own `title` and `text`; or, for one that cannot be done yet, `unavailable` and no steps. `check-site` refuses a kind or an OS the hub does not run |
| `content/docs.md`, `privacy.md`, `terms.md`, `support.md` | The public pages, in Markdown |
| `assets/` | The logo, icon, mascot, diagram, video, poster and fonts: SVG, PNG, JPEG, WebP, GIF, ICO, MP4, WebM, WOFF2, WOFF |

Only `site.json` is required: any other file left out is the built-in one's, and
`tokens.json` and `messages.json` may give only the values they change. The simplest start is
a copy of `site/default/`.

**Copy** takes a little markup: `**strong**`, `*emphasis*`, `` `code` ``, `[text](href)`
(a path on the hub, `#anchor`, `https:` or `mailto:`), and `\` before a character to take it
as it is. No HTML passes through: `<` and `>` are text, so copy can never add a script, a style
or another origin to a page. Placeholders fill in the rest: `{product}`, `{tagline}`,
`{domain}`, `{operator}`, `{operator_contact}`, `{support_email}`, `{link:<name>}`,
`{label:<provider>}`, `{term:<key>}` (a glossary word with its tooltip; `{term:<key>|shown}`
shows other words), `{pop:claim|…}`, `{pop:tools|…}`, `{pop:watch|…}` (a button that opens
that popup), and where the page has them `{mcp}` (this hub's MCP address), `{sign_in}`,
`{sign_in_url}` and `{updated}`. Copy that differs by kind of hub is an object instead of a
string: `{ "default": …, "passkeys": …, "hosted": …, "nomen": …, "unclaimed": … }`.

**Pages** are a small, strict Markdown: front matter (`title:`, `updated:`), then `#`, `##`
and `###` headings, paragraphs, `-` and `1.` lists, pipe tables, and `::: passkeys hosted`
… `:::` around what only some hubs show (`passkeys`, `hosted`, `nomen`, `other`), and
`::: analytics` or `::: no-analytics` around what a hub says only where it counts visits or
does not (blocks nest; `{retention_days}` is its `analytics.retention_days`).

**Plans** are what a hosted hub holds each tenant to, and what the Plans section shows. A
plan's `limits` name only what the hub counts and refuses past, per tenant: `projects`
(boards it binds), `machines` (machines it adds, enrolled or with a live code) and `clients`
(AI apps and terminals connected at once); a limit is a number or `"unlimited"`, and one a
plan leaves out stays the config's (`account_boards.free_limit`, the machine quota).
`check-site` refuses any other limit, so a plan never claims one the hub does not enforce.
`"default": true` marks the plan a new tenant starts on (else the first). A plan's `price` is
text, never computed; there is no billing. `content/landing.json` `plans.limits` says how each
limit reads, there and in the hub's refusals. The built-in site ships the Free plan only (D21)
with the Plans section off. `early_access.mode` is `open` or `early_access` (its note and
button then show under the plans): see "Early access and plans" below. `links.plans`, when
given, is where a refusal points for the plans; otherwise the landing page's `/#plans`.

**Assets** are served from the hub at `/assets/<name>?v=<hash>`, the first 16 hex digits of
their SHA-256, cached for a year, so the pages' CSP stays `'self'` only; a site with fonts
adds `font-src 'self'`. Only the files `site.json` and `font_files` name are served. An SVG
may hold no script, event handler, embedded page or reference to another file; one drawn
inline (logo, mascot, diagram) may hold no `<style>` or `style=` either, which the CSP would
refuse: colour it with presentation attributes or the stylesheet's classes. A diagram's ids
start with `dg-`, so the page can hold it twice.

The hub reads and checks the whole directory when it starts, rendering every page for every
kind of hub once: an unknown field, a placeholder nothing fills, a link that goes anywhere
else, a missing or unsafe asset or an unknown token stops the start with what is wrong and in
which file (`invalid_config: site.dir /etc/devagotchi/site: content/landing.json (passkeys):
nothing fills {prodcut}`). `devagotchi hub check-site DIR` runs the same check without a hub,
so a deploy can refuse a bad site before it goes live.

The running hub follows its site directory: a change is read and checked within a few
seconds and served from the next request on, with no restart and no rebuild (the stylesheet
is revalidated, so new tokens reach open browsers too). A change that does not check out is
refused the same way, and the hub keeps serving the site it had, saying why in its log.

Deployed with Docker Compose, the site directory is mounted read-only and `install.sh --site
DIR` checks and places it (`deploy/compose/README.md`, "The hub's site"); in a Proxmox
container, `hub-up.sh --site DIR` pushes it in (`deploy/proxmox/README.md`).

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

A hub hands out builds from a directory holding `VERSION`, `SHA256SUMS` and the files it
lists: the archive for each target (Linux and Windows, x86_64 and ARM64) and both installers.
The hub image carries the ones of its own commit in `/usr/share/devagotchi/downloads`
(`scripts/stage-downloads.sh` lays them down from that commit's release artifacts in CI), and
a hub uses that directory when its config names none, so a hub from the image hands out
builds with nothing configured. `"downloads": "/opt/devagotchi/current/dist"` in the config
names another (`scripts/lay-down-dist.sh COMMIT DIR` lays one down for a commit on main), and
`DEVAGOTCHI_HUB_DOWNLOADS` names one over the config. With builds to hand out, "Add a machine"
on `/account` and the AI's `machine_add` give one line per OS:

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

### Setup links the AI hands over (workflows)

An AI asked to "connect my server" calls `workflow_list`, then `workflow_start` with the
workflow and gets one link, `<hub>/go/<token>`, to give its person. Nobody signs in to use it:
the link is the credential, as the line `machine_add` hands an AI already is. It is 256 random
bits kept only as their SHA-256, bound to one tenant and one workflow, and works for
`workflows.link_ttl_secs` (30 minutes by default; 300 to 86400); once the workflow is done or
cancelled it grants nothing more, and starting the same workflow again replaces it.

```json
"workflows": { "link_ttl_secs": 1800 }
```

- **Opening it spends nothing.** A GET (a chat app's preview, a mail scanner, the person)
  shows the workflow and its steps and never a code.
- **Get my command** is a POST from that page: it issues the step's single-use join code with
  the checks Add a machine has (early access, the plan's machine limit, said plainly) and
  shows the one line for the OS picked, with a Copy button. The code comes back only in that
  browser's own HttpOnly cookie. A hub with no build for an OS offers no line for it; a Mac
  has none yet, and the page and `workflow_list` say so.
- **A phone turns on from the same link**, opened on the phone (`/go/<token>/phone`); the
  page shows the link as a QR code for the phone's camera (inline SVG, drawn by the hub).
- **Another AI app connects with its own steps** (`ai_client`): the page shows that client's
  steps from `content/messages.json` `workflow.clients` (the hub's `/mcp` address to paste, or
  the command: `claude mcp add --scope user --transport http …`, `codex mcp add … --url …`,
  Cursor's `mcp.json` entry), each with a Copy button. For an account nobody has secured, one
  press joins the client's sign-in page in that browser to the account (a session of it,
  `/go/<token>/client`), so the client's Connect lands there and not on a new account; a
  secured account signs in on the client's page as usual.
- **Keeping the account is offered after the first success, and optional**: to a browser that
  did a step through the link, once a step is done, while nobody has secured the account.
  Google or GitHub when the hub has them, a passkey otherwise.
- **The page advances on its own.** Its script asks `/go/<token>/status` every few seconds and
  moves each step's state in place, never reloading; without JavaScript, Check again does
  it. It works at phone width.

Steps are seen, not claimed: `machine` is done when a machine enrolled through it is online,
`phone` when the account has a push subscription, `ai_client` when an AI client of that
family (the name it registered with: `analytics::funnel::client_family`) has made a successful
MCP call with one of the tenant's tokens (kept per tenant and family, migration 0011), `secure_account` when the account is
secured (it never holds a workflow up), `done` when the rest are. `workflow_status` reports
each step `done`, `current`, `pending` or `unavailable` (with `why`), and `workflow_wait`
(up to 60 seconds) returns the moment one moves; `workflow_cancel` ends the link. A workflow
started, each line handed out, each step done, completion and cancelling go on the tenant's
chain. The workflows and their words are the site's (`content/workflows.json`,
`content/messages.json` `workflow`).

A linked broker that is not connected answers `broker_offline`; one that never enrolled,
`not_enrolled`; a revoked one, `device_revoked`; one with 64 calls already waiting,
`link_busy`. A refused device is told only `refused`; why is in the audit. `/healthz` reports each linked broker as
`connected` without counting an asleep laptop against the hub's own health.

## Authentication

Two modes give the hub accounts of its own, side by side in the code; a hub runs the one its
config names, and they share no tables or routes:

- `auth.mode` `hub` (below): passkey accounts in SQLite or Postgres (`src/store.rs`), the
  hub's own OAuth 2.1 server for AI clients at `/oauth/*` (32b), and the browser claim of a
  new hub (`/claim`, 16c). This is the mode `deploy/compose/` runs.
- `auth.mode` `devagotchi` (PR #79): the hub is the authorization server at `/register`,
  `/authorize`, `/token` and `/device`, accounts (`dg_…`) live in `identities.sqlite3`, an AI
  can open a provisional account that a person claims later, and Nomen is an optional
  "sign in with". Operator detail is [docs/auth.md](../../docs/auth.md).

Each mounts its own `/claim` and `/.well-known/oauth-authorization-server`; the router never
mounts both (`app_builds_in_every_auth_mode` in `src/lib.rs`).

| Mode | Bearer token | Who it is for |
|---|---|---|
| `hub` | The hub's own `dgh_` tokens, issued to AI clients at `/oauth/token` (32b); with an optional `nomen` block, Nomen tokens as in `nomen` mode. People sign in on the hub's own pages with passkeys | Any hub that should work with nothing else deployed (D25), devagotchi.com included |
| `devagotchi` | ES256 JWTs the hub signs and checks locally (`src/oauth.rs`); upstream OpenID Connect providers are optional sign-in buttons | A hub whose AIs open accounts for people who claim them later ([docs/auth.md](../../docs/auth.md)) |
| `nomen` | Any Nomen access token, opaque or JWT, checked by asking Nomen (introspection) as the hub's own API application | Shippin: your sign-in at console.shippin.cloud, relayed by mcp.shippin.cloud; `shippin-bridge` sessions |
| `oidc` | A JWT from any OpenID Connect issuer: `iss`, `exp`, `aud`, `sub` required; RS256/384/512 or ES256/384; keys from the issuer's JWKS, refreshed on an unknown `kid` at most once a minute | Self-hosters whose identity provider issues JWT access tokens |
| `dev` | A static token from a 0600 file `{ "<token>": "<subject>" }` | Tests only (plan D21): accepted when `public_url` is loopback http; a hub at an address people reach refuses to start with it |

Unauthenticated requests get `401` with `WWW-Authenticate: Bearer resource_metadata="<public_url>/.well-known/oauth-protected-resource"`;
the metadata names the issuer. A bad token adds `error="invalid_token"`. When the identity
provider cannot be reached the hub answers `503`, never `401`.

## The store

One storage layer, two backends (`src/store.rs`, ruled 2026-11-01): Postgres for a hosted
hub, which any number of hub instances share, and SQLite built into the binary for a
standalone one. The same code runs on both, and every hub test that takes a store runs on
both in CI. Where it is, first match wins:

1. `DEVAGOTCHI_HUB_STORAGE_URL` in the environment: how a Postgres password arrives, from
   the secret store (Vaultix), never from a file.
2. The config's `"storage": { "url": "postgres://hub@db.internal/devagotchi" }` (no password:
   one in the config is refused), or `"sqlite:/absolute/path"` (`sqlite:C:/…` on Windows).
3. `accounts.sqlite3` in the state directory, 0600 with its `-wal` and `-shm` files.

On Postgres the hub's tables live in the `devagotchi` schema, named by their part of the hub:
`auth_*` (accounts, passkeys, sessions, links, linked sign-ins, email links), `oauth_*`,
`tenancy_*` (tenants and their event chains) and `hub_*` (its keys). Migration 0007 moved and
renamed them once. On a Postgres something else also uses, `"storage": { "schema":
"devagotchi_hub" }` (or `DEVAGOTCHI_HUB_STORAGE_SCHEMA`) keeps the migration record in a schema
of its own, made if missing, and `public` stays as it was. One database holds one hub. On a
Supabase project this matters: Supabase's REST API serves the `public` schema to anyone with
the project's public key, so the hub's tables never go there. Use the project's **session
pooler** connection string (port 5432): the transaction pooler (6543) does not keep a session,
and the direct `db.<ref>.supabase.co` host answers on IPv6 only.

The schema is migrated at start (`migrations/<backend>/`, kept in step). On Postgres every
write runs SERIALIZABLE and runs again when it collided with another instance's; on SQLite
it takes the write lock up front.

**Tenants are kept apart by the database (Postgres).** `tenant_id` leads the key of every
table that holds a tenant's rows, and a child row's foreign key is `(tenant_id, account)`, so
no row can point into another tenant. Row-level security is enabled and forced on each of
those tables: a row is visible and writable only inside a transaction scoped to its tenant
(`set_config('devagotchi.tenant', …, true)`, which the hub sets in every transaction, from the
session, token, code, link, passkey or sign-in it was handed). Unscoped, a query sees nothing.
Finding which tenant to scope to goes through one function, `devagotchi.tenant_of`, which
answers a tenant id and nothing else. The hub's queries run as the role `devagotchi_hub`
(`NOLOGIN`, no superuser, no `BYPASSRLS`, no right to make or change tables). Every
connection switches to it with `SET ROLE`, and `/healthz` says `"store": {"tenant_rls": true}`.
This is a second wall behind the hub's own checks: a query that forgets its tenant finds no
other tenant's rows.

- **The login role** (the one in the store's URL) runs the migrations, owns the tables and
  the lookup functions, and must be a superuser or have `BYPASSRLS` (the compose stack's
  `devagotchi` is a superuser). It makes `devagotchi_hub` and grants it to itself. Where it
  may not make roles, an administrator runs `CREATE ROLE devagotchi_hub NOLOGIN NOSUPERUSER
  NOBYPASSRLS; GRANT devagotchi_hub TO <login role>;` once, before the hub starts.
- **A hosted hub** (`tenancy.mode multi`) on Postgres does not start unless its queries are
  held to row-level security. A one-owner hub starts either way and reports it in `/healthz`.
- **SQLite** has no row-level security. A standalone hub's store is its own file, and the
  same code scopes the same queries on it.
- **Rolling back across migration 0007** is the one rollback a new image cannot do by
  itself: an older hub does not read the moved tables. Restore the backup `update.sh` takes
  before every deploy ([deploy/compose/README.md](../../deploy/compose/README.md#rolling-back-across-the-tenant-migration-0007)).

## The hub's own accounts (`auth.mode hub`)

```jsonc
"auth": { "mode": "hub", "rp_id": "devagotchi.com" },   // rp_id: optional, the hub's host by default
"owner": "395290028818825218"                           // optional: an earlier provider's subject to keep
```

The hub keeps its accounts in its store and signs people in itself with passkeys (WebAuthn, verified on `aws-lc-rs`; ES256, EdDSA and RS256;
user verification on every ceremony). Nothing else needs to be running. `rp_id` is the
passkeys' relying party: the hub's host or a parent domain of it, and it can never change
for passkeys already made. Plain `http` works only on loopback, for tests.

### A hosted hub: accounts on first connect (`tenancy.mode multi`)

app.devagotchi.com is a multi-tenant control plane, never claimed. Everyone who connects
gets an account and a tenant (`t-<uuid>`) of their own, made on the spot:

```jsonc
"auth": { "mode": "hub", "rp_id": "devagotchi.com" },
"tenancy": {
  "mode": "multi",                     // or DEVAGOTCHI_HUB_TENANCY=multi; "single" is the default
  "trusted_proxies": ["172.16.0.0/12"], // whose X-Forwarded-For names the client (Caddy, the edge)
  "starts_per_hour": 8,                // new accounts per client address per hour
  "max_unsecured": 10000,              // accounts nobody secured yet, live at once
  "unsecured": { "machines": 2, "clients": 3, "idle_days": 14 }
},
"operators": ["email:you@example.com"] // or DEVAGOTCHI_HUB_OPERATORS, from the secret store
```

- **First connect makes the account.** The first OAuth authorize from an AI client, or the
  `/device` page of `devagotchi login`, opened with no session shows the same Connect (or
  Approve) page anyone sees, naming "your new Devagotchi account". Pressing it makes the
  account and its tenant, signs that browser in, and sends the code to the client: one
  click, no form, the AI working within seconds. "Sign in" is on the page for anyone who
  has an account.
- **Securing never blocks.** Afterwards the account page offers a passkey and every sign-in
  the hub has (Google, GitHub, an email link, Sign in with Shippin: below), every page
  carries a line saying the account is not secured, `/device`'s answer offers it, and
  `whoami` returns `secure_url` and `provisional_until` so the AI says so too.
- **Until it is secured** (the anonymous-then-link account upgrade: an anonymous account
  first, a real sign-in linked to it later), the account is reached only through the
  browser session that made it and the AI tokens that session approved; nothing can
  recover it; it adds `unsecured.machines` machines and connects `unsecured.clients` AI apps
  at most (per-tenant quotas); and it ends, with its sessions and tokens, once unused for
  `unsecured.idle_days`. New accounts are limited per client address (`starts_per_hour`,
  kept as an HMAC of the address, never the address) and in total (`max_unsecured`).
- **Every step is on the tenant's own chain** (`tenancy_events`, hash-chained per tenant):
  tenant and account made, session opened, each AI client and terminal connected, secured,
  each sign-in linked. The account page shows its tenant's latest steps, and only them.
- **Each tenant's things are its own.** Machine names are per tenant (stored under the
  tenant's tag, `t1a2b3c4d5e6f-laptop`; people and their AI see `laptop`), so two people can
  both have a `laptop` and neither can find the other's by trying names. Boards are the
  account's own (`board_bind`), tokens die with their account, and configured brokers need a
  grant as before. `tenant_tests.rs` drives every MCP tool and every account page as one
  tenant with another's ids, on SQLite and Postgres.
- **Early access and plans** (`accounts/access.rs`, `tenant_access.rs`). The site's
  `early_access.mode` decides whether a new tenant is let in at once (`open`) or waits
  (`early_access`). A waiting tenant's account is made and secured exactly as above, and its
  AI connects and asks `whoami` (`access: waiting`, `waiting_since`, `access_note`); every
  other tool answers with the site's one message (`content/messages.json` `access.waiting`:
  since when, that an email comes when they are in, their account page) instead of acting,
  and nothing enrolls, adds a machine or writes a board for it. Its account page says the
  same. An operator lets people in on `/operator` (below); each admission and
  plan change is on the tenant's chain and the home tenant's, the operator as actor, and a
  tenant with a verified email gets the "you're in" email when `auth.email` can send it.
  Switching `early_access` to `open` lets each waiting tenant in on its next request
  (`open sign-up`); switching back never puts anyone back. The home tenant, and every tenant
  from before early access, is in. Each tenant has a plan id (`tenancy_access`, migration
  0008, under row-level security like its other rows); a plan the site no longer lists reads
  as the default plan, said once in the log. Plans hold everyone but the owner; reaching a
  limit is refused with `plan_limit`, naming the plan, the limit and the count and pointing
  to the plans.
- **The operator page** (`/operator`, `web/operator.rs`): served only to the hub's owner,
  signed in, on a hosted hub; anyone else gets the 404 an unknown path gets. It says what
  `early_access.mode` is and how to change it (edit `site.json`, then
  `hub-up.sh --site DIR --site-only`; the mode is never a switch on the page). The waiting
  list shows each tenant's joining, sign-ins, whether it is secured, the AI app that made it
  and its last activity: check any number, pick each one's plan on its row, and one button
  lets them in. The list of those let in shows each one's plan (changed on its row), its use
  against the plan's limits, and when and by whom it was let in. Plain forms, no script,
  posts only from the hub's own page; every word from `content/messages.json` `operator`.
  The owner's account page links to it.
- **First-party analytics** (`src/analytics.rs`, migration 0009): a hosted hub counts, for
  its operator, what it serves itself: no script, no cookie, no beacon, nothing
  third-party. Each `GET` of a public page (`/`, `/docs`, `/privacy`, `/terms`,
  `/support`) answered `200` keeps the UTC day and hour, the path, the link's
  `utm_source`/`utm_medium`/`utm_campaign` (the rest of the query is dropped), the
  referring host (never its own), the device class and a visitor id: an HMAC of the client
  address (`tenancy.trusted_proxies` decides which) and User-Agent under a key held only in
  memory and replaced each UTC day, so ids never link across days; no address or
  User-Agent is stored. `Sec-GPC: 1`, `DNT: 1`, robots (`analytics/bots.txt`), requests
  with no User-Agent and the signed-in operator are not counted. Each tenant's milestones
  are written once, where each happens: made (with the AI client family from the OAuth
  client's name), secured (with the sign-in), waiting, admitted (in the account store's
  transaction), first machine, first operation (with its latest, a day at a time) and first
  board. Writes go through a bounded queue to one writer in batches (a full queue drops and
  counts, said in the log). Every hour it rolls the finished days into `hub_analytics_daily`
  (kept): each day inside `analytics.retention_days` again from its raw visits, so a visit
  written late (a backlog, another instance's queue) is counted the next pass; a day past
  the retention once more, its raw visits deleted in the same transaction. A pass that
  finds a day's visits gone keeps the totals they made, and a visit older than the
  retention is never written. The tables are
  hub-scoped, like `hub_secrets`. The default privacy page says exactly this inside
  `::: analytics` (shown only where the hub counts, with `{retention_days}`), and
  `::: no-analytics` elsewhere.

  ```jsonc
  "analytics": { "enabled": true, "retention_days": 90 }  // the defaults
  ```

  `DEVAGOTCHI_HUB_ANALYTICS` (`on`/`off`) and `DEVAGOTCHI_HUB_ANALYTICS_RETENTION_DAYS`
  override them (`install.sh --analytics off`, `--analytics-retention-days N`).
- **The analytics page** (`/operator/analytics`, `web/analytics.rs`, linked from
  `/operator`): the operator page's rule, a 404 for anyone else. A range of 7, 30 or 90
  days as plain links (`?days=`). Traffic: visitors and page views per day as an inline SVG
  chart (each day's numbers in its tooltip) with the same numbers in a table, the top
  pages, referrers and campaigns, and the device split. The funnel: visitors, then the
  accounts made in the range and how many of them were secured, let in, enrolled a machine
  and ran a command, with the rate from each step to the next. Accounts: by AI app, by
  sign-in, waiting now, and active (a command run) in the last 7 and 30 days.
  `/operator/analytics.csv?days=N` downloads the daily totals behind it (`day,metric,item,
  views,visitors`; a field a spreadsheet would read as a formula is written as text).
  Finished days come from `hub_analytics_daily`, today (and a day not rolled up yet) from
  the raw rows (`analytics/report.rs`). Server-rendered, no script, nothing third-party;
  colours and fonts are the site's tokens through `hub.css`, light and dark; every word is
  `content/messages.json` `analytics`.
- **Operators, with no claim.** Someone who signs in with an identity `operators` names (a
  verified `email:`, or `google:`, `github:`, `nomen:` or `oidc:` and the provider's
  subject) is the hub's owner, the platform operator, in the first tenant (Jesse is tenant
  #1). `devagotchi hub owner` on the host still makes the owner's passkey link.
- **No claim.** A hosted hub prints no claim code, `/` gets people started, and `/claim`
  is a 404. A one-owner hub (`single`) whose config names `operators` is not claimed either:
  its operators were decided at deploy.

Every account belongs to a tenant. On a one-owner hub there is one, and a store from
before tenants moves its accounts into it when the hub opens it (an add-only migration).

### Sign in with Google, GitHub, Shippin or an email link

Besides passkeys, a hub with its own accounts signs people in with any of these, each a
linked sign-in of the same account (`src/accounts/identities.rs`): signing in reaches the
account it is linked to; else, for an `operators` identity, the owner; else the account a
verified email already reaches; else one made on a hosted hub. From the account page, or
from a browser signed in to an account nobody secured yet, it is linked to that account,
which secures it. A sign-in already linked elsewhere is refused, never moved.

```jsonc
"auth": { "mode": "hub",
  "providers": [
    { "kind": "google", "client_id_env": "DEVAGOTCHI_GOOGLE_CLIENT_ID",
      "client_secret_env": "DEVAGOTCHI_GOOGLE_CLIENT_SECRET" },
    { "kind": "github", "client_id_env": "DEVAGOTCHI_GITHUB_CLIENT_ID",
      "client_secret_env": "DEVAGOTCHI_GITHUB_CLIENT_SECRET" },
    { "kind": "oidc", "name": "nomen", "label": "Shippin", "issuer": "https://id.shippin.cloud",
      "client_id_env": "DEVAGOTCHI_NOMEN_CLIENT_ID", "client_secret_env": "DEVAGOTCHI_NOMEN_CLIENT_SECRET" }
  ],
  "email": { "smtp_url_env": "DEVAGOTCHI_SMTP_URL", "password_env": "DEVAGOTCHI_SMTP_PASSWORD",
             "from": "Devagotchi <sign-in@devagotchi.com>" }
}
```

- Client ids may be in the file (`client_id`) or the environment; secrets and the SMTP
  password come only from the environment (set from the secret store) or a 0600 file
  (`client_secret_file`, `password_file`). A variable a provider names that is not set
  stops the hub at start.
- Each provider's redirect URI is `<public_url>/auth/provider/<name>/callback`
  (`…/google/callback`, `…/github/callback`, `…/nomen/callback`).
- The flow is the authorization code with PKCE; `state`, the verifier and the OpenID nonce
  ride in a short-lived `__Host-` cookie under the store's HMAC, so nothing is stored
  until the sign-in comes back and any hub instance finishes it. Google and any OpenID
  Connect provider are found from their issuer's discovery document; the ID token is
  checked for `iss`, `aud`, `exp` and the nonce, and an email counts only with
  `email_verified`. GitHub gives its numeric user id and its primary verified email
  (scopes `read:user user:email`). `authorize_url`, `token_url` and `api_url` point a
  `github` provider at GitHub Enterprise.
- The email link: `smtps://user@host:465` or `smtp://user@host:587` (STARTTLS required);
  `POST /auth/email` mails `<public_url>/auth/email#<token>` (256 bits, SHA-256 kept, once,
  15 minutes) and answers the same for every address; the page it opens posts the token
  from its one button, so a mail scanner that follows links spends nothing. Five links per
  address an hour (`per_address_per_hour`), twenty per client address.
- `src/provider_tests.rs` runs all of it against a stub OpenID Connect provider, a stub
  GitHub and a stub mail server.

### Claiming a new hub (16c)

A one-owner hub (`tenancy.mode single`, with no `operators`) is claimed in a browser, with a code only someone who can read its log has. A hub
whose config names no `owner` and whose owner has no passkey yet prints a claim link to its
log each time it starts (with Docker Compose: `docker compose logs hub`):

```
devagotchi hub is unclaimed: nobody can sign in as its owner yet. Claim it in a browser before 2026-11-02 18:00 UTC:
  https://hub.example.com/claim#XXXX-XXXX-XXXX-XXXX-XXXX-XXXX
```

Open it on the device that will hold the owner's passkey (or open `/claim` and type the
code; any case, dashes optional), give the owner a name and press **Claim this hub and make
my passkey**: that makes the owner's account and their passkey, and signs them in. The
landing page (`/`) of an unclaimed hub offers nothing but the claim; once claimed it offers
sign-in and says where AIs connect (`<public_url>/mcp`).

- The code is 120 random bits; the store keeps only its SHA-256 and when it stops working
  (`auth.tokens.claim_secs`, a day by default). Each start replaces it, so only the newest
  code printed works; restart the hub for a new one. It is issued once the hub listens, so
  a start that fails does not replace the code a running hub printed.
- Claiming again with the same code is how a cancelled passkey prompt is retried: it
  supersedes the earlier link. The owner's first passkey, made by a claim or by
  `devagotchi hub owner`, deletes the code, and a claimed hub refuses every claim.
- A config that names its `owner` has decided who that is (they may already reach the hub
  through Nomen): it prints no code, `/` offers sign-in, and `/claim` refuses. Their passkey
  is made on the host with `devagotchi hub owner`, below.
- Why a code and not "the first visitor owns it": a new hub is usually on the internet
  before anyone claims it. Portainer stops its setup after five minutes for this reason,
  Open WebUI documents it as an accepted risk, and Pangolin's users asked for a secret
  (issue 1053). Reading the log takes the host or its container platform, which is what
  owning the hub should take; Jenkins unlocks its setup the same way.

### The owner on the hub's host

On the host, whoever can write the state directory (the same trust as `hub enroll`) makes
the owner's link directly:

```bash
devagotchi hub owner --state-dir DIR --config /etc/devagotchi/hub.json [--name Jesse]
# https://app.devagotchi.com/owner#dgl_…   (stdout only, once; 15 minutes)
```

Open the link on the device that will hold the passkey and press **Make a passkey**; that
signs you in. The first run makes the owner's account, with the config's `owner` as its id
when it names one, so a hub that had a Nomen owner keeps every board, machine and audit row
keyed by it. Run it again to add a passkey: using the new link ends the owner's other
sessions. With `--replace`, using it also removes every other passkey of the owner: the way
to take the account back if someone else used a leaked link first. The config the command
reads must name the same owner as the hub's, or the link is refused before it is spent.
After that, `/auth/login` signs in with the passkey. Sessions last 30 days unused and 90 at
most, end when their passkey is removed, when a copy of it is seen (its counter going back),
or when the account is disabled, and are kept only as hashes. Signing out takes the hub's
own page.

Until the hub issues its own tokens (32b), a hub with no `nomen` block takes no bearer token
at `/mcp`, and refuses `trusted_issuers`. Plain `http` on loopback is for tests: its cookies
reach any other service on the same host's loopback.

Challenges are signed by the hub and stored only once used, so unanswered sign-ins cost the
hub nothing; each also rides in a `__Host-` cookie, so another site cannot sign a browser in
to an account of its choosing. The passkey pages run one script, the hub's own, under a
strict Content-Security-Policy.

## AI clients sign in with the hub itself (`auth.mode hub`)

A hub with its own accounts is its own OAuth 2.1 authorization server, so Claude, ChatGPT,
Codex or any MCP client connects to `<public_url>/mcp` with nothing else deployed:

1. The client finds the hub from `/mcp`'s 401 (`WWW-Authenticate: Bearer resource_metadata=…`),
   the protected-resource metadata (RFC 9728) and `/.well-known/oauth-authorization-server`
   (RFC 8414).
2. It registers itself at `/oauth/register` (RFC 7591): public clients only, redirect URIs
   https or http on loopback.
3. `/oauth/authorize` shows the signed-in person which app asks and where it sends them
   back; **Connect** is a POST from that page alone. Not signed in: the passkey sign-in first,
   then back to the same request.
4. `/oauth/token` trades the code (PKCE S256 required) for an access token (`dgh_…`) and a
   refresh token (`dgr_…`), both opaque and stored only as hashes. Refresh tokens rotate; a
   spent one presented again ends its whole family. `/oauth/revoke` (RFC 7009) ends a family.

`tests/clients_oauth.py` runs this with real clients against a new hub, consent approved in
Chrome with a virtual passkey: the official MCP TypeScript SDK and the Codex CLI
(`codex mcp add --url <hub>/mcp` signs in at once; tokens in Codex's own store), each then
listing tools and calling `whoami`. What each client sent is in
[`verification/clients-oauth.json`](../../verification/clients-oauth.json). Claude
(claude.ai's callback `https://claude.ai/api/mcp/auth_callback`) and Grok (grok.com →
Connectors → New Connector → Custom) register the same way and need the hub at a public
https URL.

Tokens are for this hub's `/mcp` only (RFC 8707) and die with their account. An AI on one
acts as the account's `ai` seat on boards and never has the person's standing. Lifetimes and
limits are the config's, each with a default:

```jsonc
"auth": { "mode": "hub", "tokens": {
  "access_secs": 900,       // an access token
  "refresh_secs": 604800,   // an AI connection unused (decision 4: 7 days)
  "code_secs": 120,         // an authorization code
  "max_clients": 10000,     // registered clients kept; the oldest unused make room
  "claim_secs": 86400       // the claim code an unclaimed hub prints (60 s to 7 days)
} }
```

With `auth.nomen` as well, any token that is not the hub's own goes to Nomen.

### A terminal signs in (`devagotchi login`)

`devagotchi login --hub <public_url>` (app.devagotchi.com when no `--hub` is given) is device
authorization (RFC 8628) at the endpoints the metadata names:

1. The CLI posts to `/oauth/device` as `devagotchi-cli`, a client built into the hub (any
   client registered at `/oauth/register` may ask too) and prints
   `<public_url>/device?code=XXXX-XXXX`.
2. The person opens it on any device. Not signed in: the passkey sign-in first, then back.
   The page names the app and the account and shows the code to check against the
   terminal; **Approve** is a POST from that page alone, as **Connect** is. `/device` with no
   code asks for it.
3. The CLI polls `/oauth/token` (`urn:ietf:params:oauth:grant-type:device_code`) every 5
   seconds and gets the same `dgh_`/`dgr_` pair an AI client does: for this hub's `/mcp`, in
   its own family, acting as the account's AI. It asks `/api/v1/me` whose they are and keeps
   them in `account.json` (0600); `devagotchi mcp --hub` relays an agent's session with them,
   refreshes them at the token endpoint, and `devagotchi logout` revokes them here.

Codes are 8 consonants (about 34 bits), work for 10 minutes and once; the device code and
the user code are stored only as SHA-256; polling faster than every 5 seconds is answered
`slow_down`; at most 1000 sign-ins wait at once. `tests/device_login_browser.py` runs it end
to end with the real binary and Chrome.

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
   the scope `urn:nomen:iam:org:project:id:<project id>:aud`; list their client ids in
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
| `tenant_owners` drifted from Nomen and was silently re-created | In `nomen` mode the hub keeps no copy of who owns what; it asks Nomen on every new token. In `devagotchi` mode the account is the hub's own row, and a provider is only a linked sign-in |
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
`urn:nomen:iam:user:resourceowner:id`; every member of the org finds the board, another
org never does; an account with no org gets boards of its own). One project per free org or
account, `plan_limit` beyond; the owner and the account or org IDs in
`account_boards.unlimited` have no limit. The sign-in pages, `devagotchi login` and the
protected-resource metadata (`scopes_supported`, and `scope` in the 401 challenge) ask Nomen
for exactly `openid profile offline_access urn:nomen:iam:user:resourceowner` and the hub's
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
