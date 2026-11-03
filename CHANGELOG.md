# Changelog

Releases are the operator's act: a tag `vX.Y.Z` on `main` runs `.github/workflows/release.yml`
(cargo-dist: archives for x86_64/aarch64 Linux and Windows, installers, GitHub attestations).
The version in `Cargo.toml` is bumped by the operator in the same commit as this file's next
heading. Agents draft the fragments below; they never tag.

## Unreleased

### Every hub from the image hands out builds (increment 71)
- The hub image carries the Linux and Windows builds of its own commit (x86_64 and ARM64),
  both installers, `SHA256SUMS` and `VERSION` in `/usr/share/devagotchi/downloads`. A hub
  uses that directory when its config names no `downloads`, so on a fresh stack from the
  image `machine_add`'s `curl -fsSL <hub>/join/<code> | sudo sh` and
  `irm <hub>/join/<code>.ps1 | iex` work with no extra step. `downloads` in the config, or
  `DEVAGOTCHI_HUB_DOWNLOADS` over it, still names another directory; anywhere but the image
  nothing changes.
- CI: the image is now built by `release.yml` (`custom-hub-image`, calling `image.yml`) from
  that run's own cargo-dist archives, which `scripts/stage-downloads.sh` checks against their
  `.sha256` and lays down (every target or nothing) and the jobs pass on as an artifact. No
  network fetch at hub runtime. Each arch is brought up as a hosted compose stack with no
  downloads configured, and `tests/compose_join.py` has a person's AI connect, call
  `machine_add`, run the Linux line exactly as given on the runner, and run a command on
  the machine that joined; the Windows script is checked to name both Windows builds as
  served. Only then are main and release tags pushed to GHCR.
- A build from a checkout (`docker compose up --build`) carries no builds; `hub (compose)`
  keeps proving that path.

### The operator's analytics page (increment 67)
- `/operator/analytics` on a hosted hub, linked from `/operator`, for its operator only;
  anyone else, signed in or not, gets the 404 an unknown path gets (the CSV too).
- A range of 7, 30 or 90 days, as plain links (`?days=`; anything else is 30).
- Traffic: visitors and page views per day as a server-rendered inline SVG chart (one hue
  from the site's accent: pale bars for page views, the solid part for visitors; each
  day's numbers in its tooltip), the same numbers in a table, the top pages, referrers and
  UTM campaigns, and the device split.
- Funnel: visitors, then the accounts made in the range and how many of them were secured,
  let in, enrolled their first machine and ran their first command, each with the rate
  from the step before.
- Accounts: made by Claude, ChatGPT or Codex, Grok, Cursor or another app; the sign-ins
  that secured them; how many wait for early access now; how many tenants ran a command in
  the last 7 and 30 days. How long raw visits are kept, and any records dropped.
- `/operator/analytics.csv?days=N`: the daily totals behind the page
  (`day,metric,item,views,visitors`), formula-looking fields written as text.
- Finished days are read from `hub_analytics_daily`, today and any day not rolled up yet
  from the raw rows, as the writer would roll them (`analytics/report.rs`).
- Server-rendered, working without JavaScript, nothing third-party; colours and fonts from
  the site's `tokens.json` through `hub.css`, in light and dark (and forced colours).
  Every word from `content/messages.json` `analytics`, with defaults in `site/default/`.
- Fix to increment 66's writer: a day rolled up before all its visits were written (a
  queue backlog, a slow store, another hub instance) kept the short count forever, and a
  pass that ran while another let a day's visits go could replace that day's totals with
  zeros. Every pass now rolls each day inside the retention again from its raw visits; a
  day past it is rolled once more and its visits deleted in the same transaction; a pass
  that finds a day's visits gone keeps their totals and counts only the milestones again;
  a visit older than the retention is never written. Tests run the daily pass themselves
  (the writer's pass at start is skipped under test), so it no longer races them.
- `tests/analytics_browser.py` (CI, in Chrome): a visitor reads public pages, a person
  connects an AI client, enrolls a machine through `devagotchi link` and runs a command;
  the operator's page shows the counts and the funnel move, and the CSV holds them; anyone
  else gets 404.

### First-party analytics, counted by the hub itself (increment 66)
- A hosted hub (`tenancy.mode multi`) counts visits to its public pages (`/`, `/docs`,
  `/privacy`, `/terms`, `/support`) in its own request handling: no script, no cookie, no
  beacon, nothing third-party. Each `GET` answered `200` keeps the UTC day and hour, the
  path, the link's `utm_source`, `utm_medium` and `utm_campaign` (the rest of the query is
  dropped), the referring host (never the hub's own) and the device class (mobile, tablet,
  desktop). The visitor is an HMAC of the client address (`tenancy.trusted_proxies`
  decides which) and User-Agent under a key held only in memory and replaced each UTC day,
  so ids never link across days; no address or User-Agent is stored.
- Not counted: `Sec-GPC: 1`, `DNT: 1`, robots (a maintained list in
  `src/analytics/bots.txt`) and requests with no User-Agent, assets, `/healthz`, the APIs,
  anything not answered `200`, and the signed-in operator (the web bar marks their
  requests).
- Each tenant's milestones, once each with its tenant id, where each happens: made (with the
  AI client family from the OAuth client's name: claude, chatgpt for ChatGPT and Codex,
  grok, cursor, other), secured (with the sign-in), waiting and admitted, written in the
  account store's own transaction; the first machine enrolled (`link.rs`), the first
  operation (`operation_start` or `run` that started, with its latest kept a day at a
  time) and the first board (`board_bind`) through the queue. Never for the home tenant.
- Migration 0009 on both engines, add-only: `hub_analytics_views` (raw visits),
  `hub_analytics_events` (milestones, keyed by tenant and kind), `hub_analytics_daily`
  (daily totals, kept). Hub-scoped like `hub_secrets`, granted to `devagotchi_hub`.
- Writes never slow or fail a page: a bounded queue, one writer in batches, a full queue
  drops and counts (said in the log). Every hour the writer rolls each finished day into
  daily totals and deletes raw visits older than `analytics.retention_days`.
- Config: `analytics.enabled` (default true) and `analytics.retention_days` (default 90, 1
  to 3650); `DEVAGOTCHI_HUB_ANALYTICS` (`on`/`off`) and
  `DEVAGOTCHI_HUB_ANALYTICS_RETENTION_DAYS` override them, passed through by
  `deploy/compose` and set by `install.sh --analytics`/`--analytics-retention-days` (and
  `hub-up.sh`).
- Pages may hold `::: analytics` and `::: no-analytics` blocks (shown only where the hub
  counts, or does not) and `{retention_days}`. The default privacy page now says exactly
  what is counted (no cookies, no third-party scripts, no address or User-Agent kept, a
  daily-rotating id, GPC and DNT honoured, the retention, each account's milestones) and
  keeps its "no analytics" sentence only for a hub that counts nothing.

### The default privacy page says how people actually sign in (increment 65)
- `site/default/content/privacy.md` no longer says everyone signs in through Shippin
  (id.shippin.cloud). It names every way a hub may offer (passkey, Google, GitHub, an email
  link, Shippin) and what the hub keeps for each: a passkey's public key; the account ID and
  email from Google, GitHub or Shippin, never their tokens; an email link only as a hash; and
  Shippin's tokens, encrypted, only on a hub that uses Shippin for every sign-in.
- It now also covers what the hub keeps since the hosted hub: the account made at first
  connect (temporary until secured, ending after 14 days unused), its plan and early-access
  state, the record of changes to it, the one-hour keyed hash of a network address for rate
  limits, the push address for phone notifications, and that a board belongs to the account
  unless it signs in through an organization. Content only; no code changed.

### The operator page: letting people in (increment 64)
- `/operator` on a hosted hub, for its operator (the hub's owner, signed in) only; anyone
  else, signed in or not, gets the same 404 an unknown path gets.
- A status line: what `site.json` `early_access.mode` is now, and how to change it (edit
  `site.json`, then `hub-up.sh --site DIR --site-only`). The mode stays config.
- Waiting: each tenant's joining date, its sign-ins (provider and verified email), whether
  it is secured, the AI app that made it and its last activity. Check one or many, pick each
  one's plan on its row (the site's plans, the default selected), and one button lets them
  in; their next MCP call works.
- Let in: each tenant's plan with a change-plan control on its row, its use against the
  plan's limits (machines, projects, AI apps), and when and by whom it was let in.
- Server-rendered forms that work without JavaScript, labelled for screen readers, taken only
  from the hub's own page like the other forms; every word from `content/messages.json`
  `operator`. The owner's account page links to it.
- The operator's reads go through `devagotchi.tenants_in` (ids in one state) and then one
  transaction scoped to each tenant (`accounts/operator.rs`); admissions and plan changes go
  through `admit` and `set_plan` (increment 63), on both tenants' chains.
- The hub's pages without JavaScript, in Chrome: page transitions (`@view-transition`) are
  declared only with scripting on, since a page reached by one with JavaScript off kept the
  transition's layer over it and every click landed on that layer; smooth scrolling, meant
  for the landing page, no longer animates every page's scrolling (it made controls in a
  scrolled table move under the pointer). The operator page's tables wrap long ids instead of
  scrolling sideways on a desktop screen.
- Tests: `web/operator_tests.rs` (404 for everyone else, the lists, letting in on a plan,
  the next MCP call working, plan changes, foreign posts refused, SQLite and Postgres; a
  neutral site's operator page names no brand) and `tests/operator_browser.py` (Chrome,
  JavaScript off and a phone's width for the operator page: an operator lets a waiting
  person in, and the person's next MCP call works).

### Early access and plans, enforced by the hub (increment 63)
- `site.json` `early_access.mode` now decides who gets in on a hosted hub
  (`tenancy.mode multi`). `open`: a new tenant is let in when it is made. `early_access`: a
  new person still gets their account on first connect and can secure it as before, but
  their tenant waits until an operator lets them in. While it waits, `whoami` answers
  (`access: waiting`, `waiting_since`, `access_note`) and every other MCP tool answers with
  one message from `content/messages.json` `access.waiting` instead of acting; no machine is
  added or enrolled (`machine_add`, Add a machine, `/api/v1/machines`, `/enroll`, the link)
  and no board is written. `/account` and `/api/v1/me` show the waiting state and when they
  joined.
- Switching `early_access` to `open` lets every waiting tenant in on its next request,
  recorded as `open sign-up`; switching back never puts anyone back. The home tenant and
  every tenant from before this change are in.
- Each tenant has a plan: the plan `site.json` marks `"default": true`, else the first. The
  hub enforces `plans[].limits` per tenant: `projects` (boards bound), `machines` (machines
  added, enrolled or with a live code) and `clients` (AI apps connected at once). Reaching
  one is refused with `plan_limit`, naming the plan, the limit and the count and pointing to
  the plans (`links.plans`, else `/#plans`). `check-site` refuses any other limit name
  (`idle_days` included: the hub does not count it), and more than one default plan. A plan
  id the site no longer lists reads as the default plan, logged once. `price` stays text.
- Storage: migration 0008 (add-only, Postgres and SQLite in step) adds `tenancy_access`
  (state, `waiting_since`, `admitted_at`, `admitted_by`, plan), one row per tenant, under
  forced row-level security like the tenant's other rows. The operator's lists go through
  `devagotchi.tenants_in`, which answers the ids of the tenants in one state and nothing
  else; each tenant's details are read scoped to that tenant.
- Every admission and plan change is recorded in the tenant's chain and the home tenant's,
  the operator as actor. A tenant let in gets the "you're in" email
  (`access.admitted_email`) at its first verified address when `auth.email` is configured;
  otherwise its account page says it.
- Tests: waiting tenants refused on every MCP tool, `/enroll`, `/api/v1/machines`, Add a
  machine and the board, then let in and served; both mode switches; limits at the
  boundary; default and fallback plans; the migration on both engines with row-level
  security; the email; `check-site` refusing unknown limits; the neutral site's new copy
  names no brand.

### Deploying the hub's site directory (increment 62)
- Compose mounts the site directory `DEVAGOTCHI_SITE_DIR` names on the host into the hub,
  read-only, and tells the hub only when it is set (an empty `site/` stands in otherwise,
  and the hub serves its built-in site). The hub follows the directory, so a change is
  served on `docker compose up -d hub` (or on its own, within seconds) with no rebuild.
- `install.sh --site DIR` checks the site with the hub image (`devagotchi hub check-site`)
  before anything goes live, then puts it in `site/` under `--dir` in place and names it in
  `.env`; a re-run keeps it. `--site-only` puts a site alone on a hub already installed;
  `--no-site` goes back to the built-in one.
- `hub-up.sh --site DIR` packs a site on the Proxmox host, pushes it into the container for
  `install.sh` and removes it after; `--site DIR --site-only` pushes a site change alone
  (no `--domain`, no snapshot, no reinstall).
- Tests: `tests/deploy_scripts.sh` (real Docker, CI) puts the neutral site in place, serves a
  change after `docker compose up -d hub`, refuses a broken change while the hub keeps its
  site, refuses a bad `--site` before it is live, keeps the site on a re-run and goes back
  with `--no-site`; `tests/proxmox_hub_up.py` checks the site goes into the container and
  out again, and that `--site-only` touches nothing else.

### Nothing hardcoded: the hub's site is content and tokens, from a directory (increment 61)
- Every word, name, link, colour and picture the hub shows comes from its site instead of the
  code: the landing page, `/docs`, `/privacy`, `/terms`, `/support`, the bar and every page's
  title, the sign-in email, the passkey prompt, the installed app's manifest, icon and
  notifications, the board's getting-started ticket, the terminal's client name, and the MCP
  server's name and instructions. `crates/hub/src` holds no brand, domain or operator; a test
  walks its modules and fails on one (wire names such as `X-Devagotchi-Seat` excepted).
- A site directory (`site.dir` in the config, or `DEVAGOTCHI_SITE_DIR`): `site.json`
  (product, operator, links, sign-in order and labels, the landing page's sections in order
  and on or off, the MCP display, plans, services, early access, which assets are used),
  `tokens.json` (colours light and dark, type, radii, layout, self-hosted fonts, read by the
  stylesheet as custom properties), `content/landing.json` (every section's copy, with a
  little inline markup and placeholders), `content/messages.json`, the public pages in
  Markdown, and `assets/` served from the hub at `/assets/<name>?v=<hash>`, so the CSP stays
  `'self'`. Only `site.json` is required; anything left out is the built-in site's.
- The hub checks the whole directory at start, rendering every page for every kind of hub:
  a bad one stops the start with what is wrong and in which file. No HTML passes through
  copy, an SVG may not script or fetch, a token may not leave its declaration.
- The running hub follows the directory: a change is served within seconds once it checks
  out; one that does not is refused while the site it had stays. `devagotchi hub check-site
  DIR` runs the check alone. The stylesheet and manifest are revalidated (`no-cache`, ETag)
  instead of cached for an hour.
- Today's content is the built-in site (`crates/hub/site/default/`), compiled in: with no
  directory the hub looks and reads as before. New: a Plans section rendered from `plans`
  (the built-in site has the Free plan only, D21, and the section off), a Services section,
  sections of a site's own, and radius tokens. The setup video moved to the built-in site.
- `devagotchi login` and `devagotchi mcp --hub` default to the built-in site's
  `links.hosted_hub` (app.devagotchi.com, as before).
- Tests: a neutral site (`tests/fixtures/neutral-site`) drawn on every page of a running hub,
  the email and the MCP display names nothing of the built-in one; bad directories fail start.

### `devagotchi login` against a hub with its own accounts (increment 53)
- `devagotchi login --hub URL` (and plain `devagotchi login`, app.devagotchi.com) works
  against a hub in `auth.mode hub`: it failed there, since it spoke only the routes of
  `auth.mode devagotchi`. The CLI now finds the endpoints in the hub's RFC 8414 metadata
  (whose `issuer` must be the URL given) and does RFC 8628 device authorization; a hub in
  `auth.mode devagotchi` still opens an agent account at once, as before.
- The hub (`auth.mode hub`) is a device authorization server: `/oauth/device` (advertised as
  `device_authorization_endpoint`, client `devagotchi-cli` built in), the `device_code` grant
  at `/oauth/token`, and `/device`, where a person signed in with their passkey checks the
  code against the terminal and approves or cancels. Codes are 8 consonants, live 10
  minutes, work once and are stored as SHA-256; polls faster than every 5 s get `slow_down`.
  The tokens are the hub's own `dgh_`/`dgr_` pair, as an AI client's. New table
  `oauth_devices` (migration 0004, SQLite and Postgres).
- The CLI learns the account from `/api/v1/me` (hub tokens are opaque), refreshes at the
  metadata's token endpoint and `logout` revokes at its revocation endpoint (it tried
  Nomen's path before, for hub accounts too).
- `tests/device_login_browser.py` (CI, `e2e` on ubuntu-24.04): the real binary signs in
  through Chrome with a virtual passkey, relays an MCP session as the account, refreshes,
  logs out, and is cancelled.

### AI clients connect to a hub-mode hub, proven with real clients (increment 52)
- **Connect worked in no real browser.** The consent page at `/oauth/authorize` carried the
  pages' `Referrer-Policy: no-referrer`, under which a browser sends its form POST with
  `Origin: null`; the hub refused that as "Not from this hub's page", so no AI client could
  be approved on a hub in `auth.mode hub` (app.devagotchi.com included). The page now sends
  `same-origin`: its own POST keeps the true origin, and no other site, the client's
  included, gets a referrer. The origin check is unchanged (`Origin: null` is still refused).
- **`whoami` failed in SDK-based clients.** Its output schema required `provisional_warning`,
  which the hub leaves out when false; the MCP TypeScript SDK (Claude Code, most editors)
  checks `structuredContent` against the schema and refused every `whoami` call. The field is
  now optional in the schema, and a test holds what `whoami` writes to what it advertises.
- `tests/clients_oauth.py` (CI, `e2e` on ubuntu-24.04): a new hub claimed in Chrome with a
  virtual passkey, a recording proxy at its public URL, then the MCP authorization flow
  driven by hand (RFC 9728, 8414, 7591, PKCE, 8707, 9207, refresh rotation, 7009), by the
  official MCP TypeScript SDK and by the Codex CLI (`codex mcp add`, `codex mcp login`, and
  Codex's app server listing tools and calling `whoami`), consent approved in the browser.
  Both clients are locked in `tests/clients/package-lock.json`, and Dependabot brings their
  new releases as PRs. The run is recorded in `verification/clients-oauth.json`.

### hub-up.sh re-runs over its own stack (increment 51)
- `deploy/proxmox/hub-up.sh` refused a second run: the hub it had installed holds the port,
  and it stopped on "something still listens". It now goes on when that port is published
  by its own stack's hub (`docker compose port hub` in `/opt/devagotchi-hub`), so a re-run
  updates the stack in place, as its header says. Anything else on the port still stops it
  before anything is installed. `tests/proxmox_hub_up.py` covers both.

### The hub stack's container names (increment 50)
- `deploy/compose/compose.yaml` names each container for its service alone:
  `devagotchi-hub`, `devagotchi-postgres`, `devagotchi-caddy`, no `-1`. The prefix is the
  Compose project name, so `docker compose -p NAME` still runs a second stack beside it.
  The next `docker compose up` recreates the containers under the new names; the volumes,
  and so the database and the hub's state, stay.

### Dependencies (increment 43)
- base64 0.23.1, getrandom 0.4.3, tokio-tungstenite 0.30 (the link's WebSocket), rusqlite
  0.39 (libsqlite3-sys 0.37), aws-lc-rs 1.19 (aws-lc-sys 0.47), tokio 1.54.1, uuid 1.29,
  tauri-build 2.7.4, and actions/checkout v7.0.1, actions/setup-python v7.1.0 and a newer
  taiki-e/install-action v2 in CI.
- rusqlite 0.38 moved its u64 and usize conversions behind the `fallible_uint` feature; it is
  on, so they behave as before: values up to i64::MAX are stored, and larger ones are refused.
- rusqlite stays at 0.39. 0.40 and later need libsqlite3-sys 0.38, and sqlx 0.9 accepts only
  versions below 0.38, which both link to the same SQLite.

### The landing page (increment 45)
- `/` signed out is Devagotchi's product page on every hub (`crates/hub/src/web/landing.rs`):
  the hero with a diagram of AI apps, the hub, the phone and machines dialing out; how it
  works in three steps; Desktop Commander side by side, each of its facts cited to a section
  of `docs/desktop-commander-teardown.md` (commit c2b8795) or the plan's §14; features; this
  hub's own MCP address and the connector steps; self-hosting with Docker Compose; what is
  coming (the Iroh mesh, secrets and devspaces), labelled as not built; questions; a closing
  call to action. A hub nobody owns shares the hero and still offers only the claim.
- Static HTML and inline SVG under the plain pages' CSP (no script, no inline style, nothing
  from another origin), light and dark, phone to desktop. The top bar carries the app icon's
  pet inline. `hub.css` gains the landing styles; the shared colors move a little cooler.

### Two account modes side by side (increment 42: PR #79 merged beside hub mode)
- `auth.mode hub` (passkeys, the hub's own OAuth for AI clients, the browser claim, SQLite or
  Postgres) and `auth.mode devagotchi` (PR #79: provisional AI accounts, upstream sign-in,
  `identity.rs` and `oauth.rs`) both parse and serve. Each mounts its own `/claim`,
  `/.well-known/oauth-authorization-server` and OAuth routes, so the router builds in every
  mode (a test builds it for each). `auth.mode hub` keeps its claim, owner link, OAuth for
  AI clients, SQLite and Postgres storage and `/healthz` as they were: `/healthz` reports
  `auth`, and `/api/v1/admin/settings` is served, only in devagotchi mode.
- Shared by every mode, as PR #79 made them: the `machine_add` MCP tool, `POST /enroll`,
  `/join/<code>.compose.yml` and `.host.sh`, and `/dist/SHA256SUMS`.

### Claim a new hub in the browser (increment 41, plan 16c)
- A hub with its own accounts whose config names no owner, and whose owner has no passkey,
  prints a claim link to its log at each start (`/claim#XXXX-…`, 120 random bits, only its hash stored, a day by default:
  `auth.tokens.claim_secs`; only the newest works). `/claim` takes the code, names the owner
  and makes their passkey in one step, and signs them in; the first passkey ends the code.
  `devagotchi hub owner` on the host still works, and also ends it.
- The landing page (`/`) of an unclaimed hub offers only the claim; once claimed, sign-in
  and this hub's own MCP address. `/docs` gives this hub's address and, on a hub with its
  own accounts, the connector steps with no client ID (the AI registers itself).
- Store migration `0003_claim` (SQLite and Postgres): the `claim_codes` table.
- CI runs the browser tests (`tests/claim_browser.py`, `tests/passkey_browser.py`) in the
  runner's Chrome with a virtual passkey on ubuntu-24.04, Playwright pinned by hash.
- `/docs` no longer names the hosted hub's Nomen client ID on other hubs.

### Promote a hub, and its downloads (PR #79)
- `promote-hub.sh --artifact` installs a local or CI tarball on the live hub:
  it copies `install-devagotchi.sh` into the container, stages `/dist` before
  the hub starts, rolls a snapshot back with `pct rollback --start`, and
  passes `--ssh-key`, `--ssh-opts`, and `SSH_OPTS`. `--dry-run` prints the plan.
- `/dist/SHA256SUMS` is the checksum list the hub checked. `devagotchi hub
  setup-code --config` prints the `dg_` owner id and the claim URL when an
  owner already exists. An upstream's sign-in button uses `display_name`, or
  the name with its first letter capitalized.

### Devagotchi accounts, and the hub as the authorization server
- The hub can be its own OAuth 2.1 authorization server (`auth.mode` `devagotchi`).
  Accounts, personal workspaces, and linked sign-ins live in the hub state directory.
  A verified email links a second provider to an existing account. An unverified email
  does not. Claude, ChatGPT, and Cursor register a public client here (dynamic client
  registration), use PKCE S256, and receive a JWT the hub signs and checks locally.
  Nomen is an optional upstream provider, not a requirement. `auth.mode` `nomen` is
  unchanged.
- `owner` and grants that still name an identity-provider subject are adopted once
  (`auth.legacy.issuer`, or `issuer|sub`) and stay stable. Machines enrolled to that
  subject move with the account. The config file is not rewritten.
- `devagotchi login` uses device authorization against the hub (`https://app.devagotchi.com`
  by default). `--issuer` still signs in directly with an upstream issuer.
- `/healthz` `auth` reports the identity store, the signing key, and each upstream
  provider's discovery. `docs/auth.md` is the operator page. `docs/deploy.md` and
  `deploy/linux/promote-hub.sh` describe staging, smoke, production, and rollback.
  Nothing in this change deploys.
- A devagotchi.com profile links to a Devagotchi account only when a hub token carries
  the same verified email (`apps/website/lib/hub-account.ts`).
- An AI can open a provisional account and add the person's machine before anyone
  finishes a sign-in. A remote connector's one Allow, or `POST /auth/agent/start`
  (`devagotchi login`), mints the account and tokens. `machine_add` returns the
  join line; the enrolled machine belongs to that account. A later claim attaches
  a sign-in to the same account, or merges it when the verified email already
  belongs to one. Unclaimed accounts expire. Creation is rate limited.
- Unclaimed accounts last 14 days by default (`auth.provisional` in `hub.json`).
  The owner, or a grant with `operator: true`, can change that lifetime, the
  per-IP rate, and the unclaimed cap at runtime with `GET`/`PUT
  /api/v1/admin/settings`. The saved row wins over `hub.json` on the next start.
  `whoami` warns from the live lifetime. Denied calls and changes are audited.
  Nothing in this change deploys. Staging for the promote script is a second
  instance on the Texas host, beside CT 110.

## 0.1.0 - 2026-10-31

The first release, at the operator's word (30 Oct 2026; tagged once the tool surface of
increments 34a and 34b had merged, 31 Oct). Devagotchi's local server serves 16 tools, the
hub the same file and command tools for every linked machine.

### Images the AI can see (increment 34b-3)
- `read_image` returns a PNG, JPEG, GIF or WebP file up to 3.75 MB (so its base64 stays under
  the 5 MB an AI client takes per image) as an MCP image block, with a line naming its type
  and size. The type comes from the file's first bytes, never its name; anything else is
  refused with `not_an_image`, a larger file with `too_large` at the first page. The image is
  read in the broker's ordinary 24 KiB pages (no contract change), so it works on any broker
  this release talks to. Local server and hub; the hub audits the path.
- It refuses an image over 8000 pixels on a side (`too_large`, read from the PNG, GIF, JPEG or
  WebP header): the Claude API refuses one, and a refused image stays in the conversation and
  fails every later request. Pages must join exactly and the file must keep its size and
  time, so pages of two files are never joined and a machine that misreports is refused, not
  followed; a text page that starts a byte late (an image's text metadata) is read again
  from its character's first byte. Some clients (ChatGPT, as of October 2026) do not show
  tool images to the model; the tool's description says so.

### Read by lines (increment 34b-2)
- `read_file` reads by lines, as Desktop Commander's does: `start_line` (1-based, or negative
  for the last lines) and `line_count` (default 1000, at most 5000; never more than the page's
  bytes). The answer says `start_line`, `next_line` and `total_lines`: exact for files up to
  8 MiB, and once a read from a known line number reaches the end. To read on, pass
  `next_line` as `start_line` and `next_offset` as `offset`: nothing is scanned again, so
  paging through a large file costs one page a call. A line longer than the page goes on by
  bytes. Local server and hub.
- The broker finds a start by scanning at most 256 MiB in 64 KiB chunks (`too_far` past that,
  pointing to `offset` or a tail), reads only up to the size the file had when the call
  began (a log growing meanwhile is read as it was; one that shrank answers
  `changed_while_reading`), and refuses a file that is not text with `not_text`. Escape
  counts as text now, so a coloured log reads as text by lines and by bytes.
- Every file request runs on the broker's blocking pool, so a large file never stalls jobs,
  waits or other callers.
- Contracts: `FileReadInput.start_line`/`line_count`, `FileContent.start_line`/`next_line`/
  `total_lines`; a broker from before this refuses them with `invalid_request`.

### The rest of Desktop Commander's file tools (increment 34b)
- `read_multiple_files` (up to 20 files in one call, each answered or its error reported),
  `get_file_info` (kind, size, modified and created times, read-only, Unix mode, a link's
  target, a text file's line count), `create_directory` (with missing parents; an existing
  directory is not an error) and `move_file` (same filesystem; refuses an existing
  destination unless `overwrite`, never replaces a directory, moves a link as itself), on the
  local server and on the hub for every machine.
- Contracts: `FileInfo`, `FileMkdir` and `FileMove` requests and the `Info` response; a broker
  from before this answers them with `invalid_request`.
- Under `granted`, directories are made one plain name at a time below a part that exists and
  is inside the workspace (a `..` in the part to make is refused, for `write_file`'s
  `create_dirs` too); a link is judged by where it lands, from where it is or would be after a
  move, and from its text when its target is missing (`write_file` no longer writes through a
  dangling link out of the workspace); a directory moves only when none of its links climbs
  with `..` or points outside. Moving a file onto another name of itself (a hard link, or a
  change of case where the filesystem ignores case) moves rather than reporting a
  replacement. The hub audits a move's destination, every path `read_multiple_files` asked
  for, and a refused route for `run` and `read_multiple_files`.

### The local server has the hub's tools, and runs as you (increment 34a)
- `devagotchi mcp` serves `run`, `read_file`, `list_directory`, `write_file` and `edit_file`
  (the hub's tools, names and inputs, without the machine's name) and `operation_list`
  beside the operation tools. An AI runs a command, or reads, writes or edits a file, in one
  call. The server is named `devagotchi` (was `desktop-bridge`), its instructions steer an AI
  to `run` and the file tools, and every tool has a title and read-only/destructive hints.
- A first `devagotchi start` (and so `setup` and an AI's first `devagotchi mcp`) writes
  `exec_policy: account`: the broker runs what the person's account can, shell lines included,
  as Desktop Commander does (trust by default, ruled 30 Oct 2026). An existing `broker.json`
  is kept; `"exec_policy": "granted"` narrows it again. Server installers keep their own default.
- `run`'s composition (start, one held wait, read) lives in `devagotchi-mcp` and serves both
  the local server and the hub. On the way, for both:
  - the cursor `run` gives for a command still running points past the output it already
    returned (it named the start of the last page, so following it repeated that page);
  - it reads only the output written by its first read, so a command still writing fast is
    not chased past the wait it was given, and stopping the call stops the reading too;
  - a broker that runs only granted programs refuses `run` with what works instead
    (`operation_start` with an executable, or `exec_policy` `account`), not with a field
    `run` does not have.

### Install in one line, for a person or their AI (increment 33b)
- README "Install": one line per system that installs `devagotchi` for the user from the
  latest release (cargo-dist's installers, `~/.local/bin`, no administrator rights) and runs
  `devagotchi setup`. Each line stops before `setup` when the download or the install fails
  (`bash -o pipefail`; PowerShell `try … catch { throw }`). The Windows line works the same
  from PowerShell, Command Prompt and Git Bash (the shell Claude Code uses there), and under a
  PowerShell 7 session, whose module path otherwise stops Windows PowerShell loading the
  installer's cmdlets. What it changes, said plainly for an AI to tell the person first (the
  binary, the PATH lines the installer adds, client configs, a background broker whose granted
  tools can do whatever the account can); how to update and to undo, in order; another MCP
  client's config; and which installer checks a checksum. `llms.txt` says the same. Marked
  unpublished until v0.1.0 exists.
- `devagotchi setup`/`connect` print the server's absolute command before saying they found
  no client, so another MCP client can be given it.
- The release gate (`.github/workflows/install-check.yml`, which `host` needs): on
  ubuntu-22.04, ubuntu-22.04-arm, ubuntu-24.04, ubuntu-24.04-arm, windows-2022, windows-2025
  and windows-11-arm, the README's own line, run from every shell there, installs from the
  very installers and archives `host` publishes, served on 127.0.0.1. Each shell must fetch
  its system's own archive (Git Bash on ARM64 Windows gets the x64 build, as documented), a
  failed download must stop the line before `setup`, `setup` sets up Claude Code (a stub
  records the call), Codex and Claude Desktop and its check passes, the broker is killed as a
  reboot would, and the command Codex was given starts it again and runs `git --version` to
  the end (`tests/install_check.py`, which runs only on a GitHub-hosted runner or a declared
  throwaway home, and stops only processes of its own install). A target dist builds with no
  runner fails it. `host` publishes a release only when every runner passed; pull requests
  run it too.

### One step for an AI client, no account (increment 33a)
- `devagotchi mcp` with no broker answering starts this user's broker in the background
  (`devagotchi start --background`) and uses it, so an AI client's whole config is
  `devagotchi mcp`, as Desktop Commander's is one `npx` line. The broker
  (`devagotchi_core::execution::detached`, native code in the platform modules):
  - has a session of its own (Linux), or no console, a process group of its own and is out of
    the caller's job where the job allows it (Windows), so a client closing its whole process
    tree leaves it running;
  - inherits nothing but the null device and its log: every other descriptor is closed on
    exec (Linux), and Windows gets an explicit inherited-handle list, so an AI client's pipes
    are never held open;
  - runs in its state directory with a pared-down environment: what programs need to find
    home, temp and tools, plus the SSH agent, proxies, extra CA certificates and toolchain
    homes jobs need; an AI client's own variables, tokens in an MCP `env` block among them and
    who it acts as (`DEVAGOTCHI_ACTOR`), do not reach it or its jobs (a broker started in a
    terminal keeps that terminal's whole environment);
  - writes to `<state>/broker.log` (0600); a broker that stops at once is reported at once with
    what it wrote (not an earlier start's lines), and one that holds the state but does not
    answer is reported as such.
  The state directory is made by the IPC layer that checks it, which also fixes `devagotchi
  start` on Windows for administrators (a directory they make is owned by Administrators and
  was refused). Two starts racing meet at the broker's instance lock; a broker is found by
  asking its endpoint, never by a remembered PID.
- `devagotchi start --background`: the same start, detached; returns once the broker answers.
- `devagotchi setup`: `connect` for every AI client found, with this machine's broker when no
  account is on file instead of asking to sign in first; an account whose sign-in ran out keeps
  its hub. Codex gets `startup_timeout_sec = 30`, since a first start may start the broker.
- `DEVAGOTCHI_BROKER_CONFIG` names the broker's config file alone. The Windows installer sets
  it and `DEVAGOTCHI_STATE_DIR` for the user, so plain `devagotchi mcp` and `setup` find its
  broker on D: instead of starting a second one, while the sign-in stays in `%APPDATA%`; it
  stops a broker started either way before replacing binaries.
- `devagotchi start` writes a first config through a temporary file of its own process.
- Windows: a broker started by an elevated administrator can be started again by the same user
  unelevated or from Git Bash. Its lock and journal, owned by the Administrators group when made
  elevated, were refused (`lock_permissions`) where the token's owner is the user. A private
  file may now be owned by that group too; its DACL must still grant the user alone.

### devagotchi.com sign-ins reach the hub (increment 31)
- `trusted_issuers: [{ "issuer", "audiences" }]` in the hub config: more OpenID Connect
  issuers whose signed JWTs the hub accepts next to `auth` (Nomen stays the primary). A token
  is checked by the issuer its `iss` names, with that issuer's keys; every other token goes to
  `auth`. https only, no duplicates, never beside `auth.mode dev`. Discovery runs at start.
- `POST /api/v1/machines` `{ "name" }`, bearer only: the same one-time owned code "Add a
  machine" issues, plus the hub's `/join` lines when it serves downloads, `Cache-Control:
  no-store`. The hub's cookie is not accepted there, so another site cannot mint codes.
- Verified: `cargo test -p devagotchi-hub` (121, including ES256 routing, a forged-key token
  refused, config rules, bearer-only issuing and the code joining for that account only).

### The hub speaks Nomen's own names (increment 31)
- Every scope the hub and `devagotchi` ask Nomen for, and the org claim the hub reads, use
  Nomen's spelling: `urn:nomen:iam:user:resourceowner` (claim
  `urn:nomen:iam:user:resourceowner:id`), `urn:nomen:iam:org:project:id:<project>:aud`, and
  `urn:nomen:iam:org:project:id:nomen:aud` for Nomen's own APIs in `connect-nomen`. RFC 9728
  `scopes_supported` follows, so MCP clients ask in the same spelling. The upstream
  `urn:zitadel:` names were accepted only through Nomen's compatibility layer; the hub no
  longer depends on it, and no longer reads the org from an identity provider that sends
  only the upstream claim.

### The hub connects itself to a Nomen (increment 30)
- `devagotchi hub connect-nomen --state-dir … --config … --client-id ID --project ID
  [--issuer URL]` joins the hub to a Nomen with its owner's approval and nothing else: it
  shows a code to confirm on any device through the `devagotchi-cli` app of the Devagotchi
  project (made by Nomen's deploy), then adds the hub's API app and browser app to that
  project with the approver's token. The API app's key is generated by Nomen, handed back to
  this process only and written to the state directory (0600); it never travels anywhere
  else. The config gets `auth` (mode `nomen`, the project as audience, the CLI and web apps
  as clients), `owner` (whoever approved) and `web`, is checked as a hub config before it
  replaces the old one (written whole, 0600) and the old one is kept beside it.
- It also makes `devagotchi-ai-connectors`, a public PKCE app with the claude.ai and
  claude.com MCP callbacks (no secret), lists it as a client and prints its id: the OAuth
  client id a claude.ai or claude.com custom connector to `<public_url>/mcp` takes.
- Again on the same project: the apps are reused, a fresh key is added and the clients
  already listed stay. Someone other than the owner approving is refused and changes nothing;
  so is a declined code.
- `account::Issuer::login_with_scope`: the device sign-in with a caller's scope.

### One line joins a machine (increment 29b)
- With `downloads` in the hub config (a directory of `VERSION`, `SHA256SUMS` and the build the
  hub runs), "Add a machine" shows one line per OS: `curl -fsSL <hub>/join/<code> | sudo sh` on
  Linux and `irm <hub>/join/<code>.ps1 | iex` on Windows. The script `/join/<code>` serves
  downloads that build from the hub's `/dist`, checks every file against the SHA-256 written
  into it, installs, makes the machine's key, enrolls (the code on stdin or in the
  environment, never an argument), sets `exec_policy: account` and starts the broker and the
  link at boot. Fetching it does not spend the code; enrolling does; a dead code's script only
  says so. The hub hashes the listed files at start and leaves `/join` off, reported in
  `/healthz`, when one does not match.
- `scripts/lay-down-dist.sh COMMIT DIR` lays down that directory from GitHub's build of a main
  commit: the four archives through `fetch-main-build.sh` (checksum and attestation), the
  commit's installers, `VERSION` and `SHA256SUMS`.
- `install-devagotchi.sh --exec-policy granted|account` and `install-devagotchi.ps1
  -ExecPolicy`; the Windows installer also takes the code from `DEVAGOTCHI_ENROLL_CODE`.
- CI: the Linux host-install job joins a bare account with the one line from a hub serving the
  runner's own build and runs a real operation through it.

### Add a machine from the browser (increment 29a)
- The Machines page (`/account`) has **Add a machine**: a signed-in person names a machine
  and gets a one-time code for it, shown once, valid 15 minutes, with the installer line for
  Linux and Windows. The machine that enrolls with it belongs to that account: only it sees
  and reaches the machine (`machines`, `whoami`, every routed tool) without anyone editing
  the hub's config, and only it can **Revoke** it. Names the hub config uses, names another
  account holds (or holds a live code for), and more than 16 machines per account are
  refused. The owner's phones are told when the machine joins, with its key's fingerprint.
- The link admits a machine the config does not name only with an account's code or as an
  account's enrolled machine; an operator's code still joins only a configured broker.

### Sign-ins survive a hub restart during a Nomen outage (increment 27b)
- The sign-ins Nomen confirmed are kept in `signins.sqlite3` in the hub's state directory:
  the token's SHA-256 (never the token), the account, and until when it may be honoured
  (the token's expiry, at most 12 hours after Nomen last confirmed it). A hub restarted
  while Nomen is down takes them back and honours them exactly as before the restart;
  while Nomen answers, Nomen decides, and its answer that a token is no longer good
  deletes the row. A store that cannot be written is logged once and never fails a
  sign-in.

### `devagotchi connect`: Claude Code, Codex and Claude Desktop in one command (increment 28a)
- Each client found on the machine (or named) gets an MCP server `devagotchi` running this
  binary: `mcp --hub` (the hosted hub as the signed-in account; the default once
  `devagotchi login` ran) or `mcp` with `--local`. Claude Code through its own
  `claude mcp add --scope user`; Codex as `[mcp_servers.devagotchi]` in config.toml
  (`CODEX_HOME` honoured, `tool_timeout_sec = 120` because a wait is held up to 50 s);
  Claude Desktop as `mcpServers.devagotchi`. Every file is backed up and written whole;
  running it again replaces Devagotchi's entry and nothing else.
- Then the server is started once and asked for its tools: the answer is whether it works.
- The hosted hub's default URL is `https://app.devagotchi.com` (was devagotchi.com, which
  now redirects there; the relay never follows redirects, so `mcp --hub` and
  `board bind --hub` without a URL failed).

### The hub keeps working while Nomen is down (increment 27a)
- 26 Oct 14:45 UTC: Gold's edge stopped answering, so Nomen did too, and every call to the
  hub on Texas waited 10 s on it and then failed; Claude's connector timed out. Now a token
  Nomen confirmed keeps working through such an outage, until the token's own expiry and
  never more than 12 hours after Nomen last confirmed it. A token Nomen never confirmed
  gets 503 with Retry-After at once instead of after a 10 s wait.
- After one failure the hub stops making every caller wait on Nomen: one request at a time
  tries again, after 2 s doubling to 30 s. Nomen gets 3 s to connect and 5 s to answer
  (was 5 and 10).
- The outage is logged once when it starts and once when it ends, with how long it lasted
  and how many requests were honoured meanwhile; per request, at most one line every
  30 s. `/healthz` gains `issuer`: reachable, down since, last answer, last error.

### Board waits wake on the change itself (increment 26b)
- Every board write that records an event advances the board's change signal after it
  commits. `board_wait` and `approval_wait` sleep on it instead of re-reading the store
  every 500 ms, so a note, a claim or a person's decision on the phone reaches the waiting
  agent the moment it lands. A write that records nothing (an empty reap) wakes nobody.
- Lapsed leases are expired when they lapse: the reaper sleeps until the first lease on
  any open board ends, or until a board changes, instead of sweeping every 60 s.

### The inspector waits instead of polling (increment 26e)
- New broker request `operations_changed {after_revision?, timeout_ms}`: answered when an
  operation is admitted or retired or changes state (output alone does not count), or at
  the timeout with the same revision; a revision the broker does not have (it restarted)
  answers at once.
- The desktop inspector re-lists only when that answer changes, instead of every 3 s,
  and follows the selected command's output with `operation_wait` (output past its
  cursor, or settlement) instead of reading every second; draining pages no longer
  pauses 100 ms between them. After an error it retries in 3 s.
- The desktop crate builds against the current contracts again: its response-kind match
  lacked the file responses added in 24d and 24e.

### Process exits are events on Windows (increment 26d)
- The root's exit is the wait on its process handle (registered with the system thread
  pool by the runtime), not a 20 ms look. Each Job Object gets its own I/O completion
  port, associated before the root is assigned; a listener thread advances a signal on
  every member-exit and active-process-zero notice, and the broker queries the job only
  then. Windows documents job notices as not guaranteed to arrive, so while members
  outlive the root the job is also queried once a second (never while the root runs).
- Termination waits for the job's notice instead of looking every 20 ms.

### Process exits are events on Linux (increment 26c)
- The broker no longer wakes every 20 ms while a command runs. On Linux it holds a pidfd
  for the unreaped root (readable when it exits, Linux 5.3+) and sleeps until the root
  ends, a stop is requested or the deadline passes. Members that outlive the root are
  waited for the same way: /proc is read once, every member found gets a pidfd (checked
  to still be a member after it is opened; 64 at a time), and /proc is read again only
  after they have all exited.
- Cancellation sends SIGTERM and moves to SIGKILL as soon as the group has left, instead
  of always waiting 200 ms; settlement after SIGKILL is the same event wait, bounded by
  one second as before.
- A kernel or sandbox without pidfd_open keeps the previous 20 ms look. Windows keeps it
  until increment 26d.

### Waiting, not polling: `operation_wait` (increment 26a)
- The broker answers `operation_wait` the moment an operation settles (or, with
  `output_after`, has output past that sequence), or after `timeout_ms` (at most 50 s)
  unchanged. It sleeps on one change signal that every state change, output frame and
  cancellation advances; nothing is re-read on a timer. At most 16 waits are held at once
  (`wait_busy` beyond), so ordinary requests always get a connection.
- Local IPC and the machine link allow a held request its wait on top of their usual
  timeouts; the link keeps waits in their own pool of 8 so they never crowd out calls.
- The hub's `run` makes one held request instead of asking every 50–500 ms, and both the
  hub and the stdio adapter offer `operation_wait`, so an AI waits in one call instead of
  calling `operation_get` again and again. The broker's own tests and the protocol and
  end-to-end harnesses settle operations through it.

### Our servers run GitHub's builds (increment 25a)
- The release workflow also runs on every push to `main`: it builds, attests and keeps the
  archives for each target as workflow artifacts, without publishing a release (a version
  tag still does that). `scripts/fetch-main-build.sh COMMIT` downloads a commit's build,
  checks every archive's SHA-256 and verifies its GitHub attestation, and hands the
  directory to `install-devagotchi.sh --version main-<short> --artifact-dir`.

### Changing files: `write_file`, `edit_file` (increment 24e)
- `write_file` overwrites (the default: written beside the file and renamed into place, so
  a reader never sees half of it; permissions carry over), creates (refused when it exists)
  or appends, up to 32 KiB per call, from text or base64, optionally creating parent
  directories (under `granted`, only below an ancestor inside the workspace). With
  `expected_sha256` it refuses (`changed_since_read`) when the file changed since.
- `edit_file` replaces exact text in a UTF-8 file (up to 8 MiB) only when it occurs the
  expected number of times (default once); otherwise nothing changes and the answer says
  how many times it occurs. Both return the file's new size and SHA-256.

### Files in one call: `read_file`, `list_directory` (increment 24d)
- The broker answers `file_read` and `file_list` directly (no operation, nothing
  journaled), under the same reach as execution: inside the workspace under `granted`,
  wherever its account can read under `account`. A read returns up to 24 KiB from an
  offset (negative: from the end), as text when it is UTF-8 text (never splitting a
  character) and as base64 otherwise, with `next_offset` and `eof`. A listing goes up to 3
  levels and 1000 entries, directories first, symbolic links listed and never followed.
- The hub's `read_file` and `list_directory` tools route them by machine name, audited
  per call with the path.

### `run`: a command and its output in one call (increment 24c)
- The hub's `run` tool takes a machine, a command line and optional `cwd`, `wait_seconds`
  (1–50, default 30) and `timeout_seconds` (1–300, default 300). It starts the command as a
  broker operation, waits, and returns its state, exit code and output (stdout and stderr in
  order, the last 32 KiB). A command still running when the wait ends keeps running and
  comes back with its `operation_id` and how to follow it. One audit entry per `run`. It
  needs a machine whose broker runs under the `account` policy; a `granted` one refuses it
  with `command_not_allowed`. The hub's instructions now send an AI to `run` first.

### Commands as the account (increment 24b, ruled 25 Oct)
- A broker's `exec_policy` (in `broker.json`) is `granted`, the default and the old
  behaviour, or `account`: the broker runs anything its OS account can, in any directory,
  with that account's environment, and takes a `command` line for the machine's shell
  (`$SHELL` when it is bash, zsh, sh, dash or ksh, else bash, else `/bin/sh`; Windows
  PowerShell with `-EncodedCommand`). `operation_start` carries `command` or `executable` with
  `args`, never both; a `granted` broker refuses `command` with `command_not_allowed`.
  The account's permissions are the boundary: a broker that takes commands runs as an
  unprivileged account. Approvals for risky steps and the installers' default follow.

### Bounded history, not a lifetime quota (increment 24a)
- A broker kept every operation it ever admitted and refused all new work once it held
  `max_operations` (64 by default), so a machine stopped taking commands after its 64th.
  Now the operation that finished longest ago is retired to make room; admission is refused
  only while every held operation is still running. A retired operation's ID answers
  `not_found`, and its idempotency key is remembered (the newest 4096) and refused with
  `idempotency_key_retired`, so a late retry never runs the work twice.

### One binary (increment 13a)
- `devagotchi`: one organised command tree over the same libraries — `devagotchi start`
  (this user's broker; the first run writes `broker.json` with the workspace and the
  toolchains on PATH), `devagotchi mcp` (the adapter an agent runs; finds that broker through
  `<state>/endpoint`), `devagotchi board …`, `devagotchi broker run …`, `devagotchi hub
  serve|enroll|revoke|devices`, `devagotchi link enroll|run`, `devagotchi version`.
- The clean cut (13b): `devagotchi` is the only binary the release ships and the installers,
  harnesses, CI and docs call (`bridge-broker`, `bridge-mcp`, `devagotchi-link` and
  `devagotchi-hub` are gone; their crates are libraries). The installers' units run
  `devagotchi broker run`, `devagotchi link run` and `devagotchi hub serve`; Claude Desktop
  and Claude Code are registered with `devagotchi mcp`.

### The account (increment 14a, D22)
- `devagotchi login`: sign in with a Shippin account from a terminal with no browser —
  OAuth device authorization (RFC 8628) against Nomen; the code and URL are shown, the
  tokens land in `account.json` under the config directory, owner-only. `devagotchi whoami`
  (`--json`) names the account and refreshes a token that is due; `devagotchi logout`
  revokes both tokens and forgets the account; `devagotchi login --local` records the
  accountless choice. `devagotchi start` says which it is until one is on file. The hub
  accepts the token `login` lands when the CLI's client id is in `auth.clients`.

### Sign in and out from anywhere (increment 22)
- Every hub page has a top bar: signed in, Approvals, Machines and a one-tap Sign out;
  signed out, one Sign in that returns to the same page. `/` explains Devagotchi and offers
  Sign in with Shippin instead of bouncing a stranger to the identity provider.

### Org boards (increment 21a, B13)
- A project's board belongs to the org the identity provider proves the account belongs to
  (Nomen's `urn:zitadel:iam:user:resourceowner:id`): `board_bind` by any member makes or
  finds it, every member and their agents work it, another org never sees it. Seats are
  `<account>/<seat>` on an org's board; the free limit counts the org's projects; `whoami`
  says the org. Accounts with no org keep boards of their own.
- Web sign-in, `devagotchi login` and the RFC 9728 metadata (`scopes_supported`, and `scope`
  in the 401 challenge) ask for exactly `openid profile offline_access
  urn:zitadel:iam:user:resourceowner` and the hub's audience.

### The command line bound to the hub (increment 15b, B11)
- `devagotchi board bind [--hub URL] [--project NAME]`: the hub makes (or finds) the account's
  board and `.devagotchi/config.json` remembers the hub; every `board` command and hook here
  then runs on the hub as the account on file, the agent's name as its seat; `board unbind`
  goes back to the local store. The hub's refusals keep their codes; `not_signed_in`,
  `signed_out` and `hub_unreachable` say what to run. `devagotchi mcp --hub` sends the seat
  (`DEVAGOTCHI_ACTOR`). `board_list` takes `@me`.
- `devagotchi hub board …` is gone; `devagotchi board …` is the one command line.

### Account boards on the hub (increment 15a, D23)
- `account_boards` in the hub config: `board_bind {project}` makes the calling account's own
  board (one per account-project under `dir/<account>/`), listed first in `whoami.boards`; a
  free account binds one project (`plan_limit`; the owner and `unlimited` accounts have no
  limit); a shared board's name cannot be bound (`name_taken`); a hub without the section
  answers `not_offered`.
- Seats: `X-Devagotchi-Seat: <name>` makes one account's agents distinct actors on its own
  board, and `<account>/<seat>` on a shared one; never a person; fences kept per seat.

### The account in the agent's session (increment 14c)
- `devagotchi mcp --hub [URL]`: the stdio MCP server an agent already runs, relaying the
  session to the hosted hub's `/mcp` (default https://devagotchi.com) as the account on
  file — the token stays on this machine and rotates underneath the agent; a refused,
  expired or missing account is answered on the request's id with what to run.

### Dev tokens are for tests (increment 14b, D21)
- The hub accepts `auth.mode dev` only when `public_url` is loopback http; at an address
  people reach it refuses to start and names the alternatives (`nomen`, `oidc`).
- `install-devagotchi.sh --role hub --auth-config FILE`: the `auth` object (and `owner`) from
  FILE, the identity provider's key installed as the hub account's own 0600 file, no tokens
  minted; `--person`/`--agent` are account IDs. A reachable `--public-url` without it is
  refused before anything is written.

### The agents' board (increments 7–8)
- `devagotchi-hub board …`: a per-project ticket board in `<project>/.devagotchi/board.sqlite3`
  — hash ids `<prefix>-xxxx` with hierarchical children, workflow lanes (ready → in_progress →
  review → reviewing → done; the reviewer is never the author), claims with leases, fences,
  heartbeats, expiry and a reaper, append-only notes, a sha256 event chain, `export`/`verify`.
- The command line for agents: `init --agents-md`, `prime`, `claim`, `beat`, `note`, `done`,
  `handoff`, `release`, `force` (people only), `set`, `dep`, `held`, `hooks`; JSON off a pipe,
  exit codes by failure class, a `next` hint on every error, a claim cache so fences are never
  copied by hand.
- The board over MCP: seventeen `board_*` tools on the hub's `/mcp`, `boards` in the hub
  config, grants with `boards` and `human`, `whoami` reporting both, the fence remembered per
  subject, every call on the audit chain, lapsed leases reaped every minute.
- The hook pack: `board hooks` writes `.devagotchi/hooks/{session-start,beat,stop}.sh` and
  prints the Claude Code and Codex hook configuration — prime at session start, heartbeat
  after tool calls, no stopping while holding a ticket.
- Contract and decisions: `docs/agent-board.md` (B01–B09; B02 per-project store and B06 ACP
  for all five seats ruled in chat, ADRs pending through the component).

### Deployment (increments 7, 8, 10)
- ARM64: release builds, installers and CI on aarch64 Linux and Arm64 Windows.
- Release hardening: fat-LTO, single-codegen-unit, stripped `dist` profile; every GitHub
  Action pinned to a commit SHA; Dependabot for crates and actions.
- `install-devagotchi.sh --role hub`: the hub on a server as a user service on loopback,
  `hub.json` written once, project boards initialised, bearer tokens minted and shown once,
  Caddy in front with automatic TLS (`--caddy`). CI runs it on a real systemd host behind Caddy.

### Absorbing the Go hub (increment 9)
- `docs/devagotchi/README.md`: the ledger of what `shippinAI/agent-hub` had, what is ported,
  partial or pending, plan row D19, and the one-command import of its design documents.
