# Plan: AgentHub as its own surface, editions for other brands, and linking to a network

Status: PLAN for Jesse's review (2026-11-06). Nothing here is built. Each phase gets its own
code-anchored spec for approval before any agent works on it. The point of this document is
to settle the shape **before** building, so no later brand, domain or licence choice forces a
rewrite.

Ruled 2026-11-06 (Jesse): Devagotchi is the self-hosted product with its own GitHub org; the hosted, network-connected version evolves in a private enterprise repo (§5). AgentHub gets its own boundary at **agenthub.devagotchi.com**; the
brand/name must be changeable (with a UI) so the same service can be offered under another
name (e.g. **shippin.online**) and can **link to the Shippin Network**; likely offered openly
under BSL.

## What exists today (main, 2026-11-06)
- One hub process (`crates/hub`) serves everything at one origin, `config.public_url`
  (`config.rs` ~36): the landing, `/app`, sign-in, OAuth (`web/oauth.rs`), and **one** MCP at
  `/mcp` (`lib.rs` ~907) with all 45 tools: machines + files + run + **board** + workflows +
  offers (`hub.rs` ~878, tool routers summed).
- OAuth 2.1 with the protected-resource metadata document (`/.well-known/oauth-protected-
  resource`, `web/routes.rs`), DCR, PKCE: the standard MCP authorization shape.
- Brand and words come from a site directory (`site/default/`, `DEVAGOTCHI_SITE_DIR`), read at
  start; editing means a file change and a restart. Only the Devagotchi one exists.
- Multi-tenant accounts, Nomen as an optional upstream sign-in, "Shippin Network" offers
  (`offers.rs`), removable by config.
- Workspace licence: `Apache-2.0` in `Cargo.toml`.

## The shape (industry-standard, so nothing pivots later)

### 1. Surfaces, not separate products
One authorization server, several **protected resources**, each on its own hostname. This is
how the MCP spec's authorization model (RFC 9728 resource metadata + RFC 8707 resource
indicators) and every large platform (GitHub `api.`/`uploads.`, Atlassian per-product hosts)
separate services without separate identities.

| Surface | Devagotchi | Shippin edition | What it serves |
|---|---|---|---|
| landing | devagotchi.com | shippin.online | marketing, docs, llms.txt |
| app | app.devagotchi.com | app.shippin.online | sign-in, OAuth server, `/app`, machines MCP at `/mcp` |
| agenthub | agenthub.devagotchi.com | agenthub.shippin.online | the board MCP at `/mcp`, board UI |

- Each MCP has its **own resource identifier** (`https://app.…/mcp`, `https://agenthub.…/mcp`)
  and its own resource-metadata document naming the **same** authorization server
  (`app.…`). One sign-in, one account, two connectors; a token for one is not valid at the
  other (audience-bound), which is the "clear boundary".
- **Same process to start** (one deploy, routed by `Host`), so it is not a second service to
  run; it can be split into its own deploy later without changing any URL or client.
- Main MCP keeps machines/files/run/workflows; AgentHub MCP keeps `board_*`. Each tells the AI
  about the other: the machines MCP gets `agenthub_connect` (returns the AgentHub connector URL
  and a **paste-ready line** for the user: "Add a connector: https://agenthub.devagotchi.com/mcp"),
  and AgentHub's instructions name the machines MCP. Existing clients keep working for one
  transition period: `board_*` stay on `/mcp` marked deprecated, then are removed.

### 2. Editions: the brand is data, not code
An **edition** = everything that makes a deployment look and speak as one brand: name, logo,
colours, words (today's site directory), the surface hostnames above, sign-in options, plans,
and which network it belongs to.
- Stored in the hub's database (the site directory stays as the **defaults** an edition starts
  from), versioned, with an **operator UI** (`/operator/edition`): edit name, logo, colours,
  words, domains, preview, publish, roll back to an earlier version. No restart.
- One image; each brand is its own deployment with its own database and edition record
  (devagotchi.com and shippin.online never share accounts unless they federate, below).
- Hostnames come only from the edition; nothing in code names a domain (already a rule).

### 3. Networks: how an edition links to the Shippin Network
- A **network** is configured on an edition: its identity provider (Nomen/OIDC), its offers
  catalogue (today's `offers`), and a link endpoint.
- Linking an account = OIDC account linking to the network's IdP (already how "Sign in with
  Shippin" works), so the service can evolve into the network without migrating accounts.
- An edition with no network shows no offers. Devagotchi's default edition names the Shippin
  Network (as ruled 2026-11-03); shippin.online's *is* the network's own edition.

### 4. Licence, decided before going public (not legal advice)
- BSL 1.1 is source-available, not "open source" by the OSI definition; it converts to an open
  licence (usually Apache-2.0) on a Change Date per version. Choose the Additional Use Grant
  (e.g. "free to self-host; no competing hosted service") and Change Date before the first
  public release: releases already out keep their licence.
- Today the workspace says Apache-2.0. Moving to BSL means changing it before the public
  release, and either keeping all code your own or having contributors sign a CLA so you can
  relicense. Third-party dependency licences must allow it (`cargo deny` already checks them).
- The "public core + private repo for commercial features" ruling (2026-11-03) fits this: the
  public repo under BSL, the commercial hosted features private.

### 5. Two repos, one product line (ruled 2026-11-06)
Jesse: Devagotchi gets **its own GitHub org**. **Devagotchi is the self-hosted product**: people
deploy it on their own VM as a central connection point they own (their privacy). The version
that connects to and offers Shippin, Vaultix and the rest evolves in the private enterprise repo.

| Repo | Visibility | Holds | Builds |
|---|---|---|---|
| `devagotchi/devagotchi` | public (BSL, §4) | broker, link, stdio MCP, hub with the machines MCP **and AgentHub**, the default edition, installers, docs | the self-host image and binaries |
| `devagotchi/devagotchi-enterprise` | private | SSO + org admin (SAML/OIDC per org, SCIM, roles); audit + compliance (export, retention, SIEM streaming); the editions/white-label editor (§2); Shippin Network hosting (plans, billing, network edition, Vaultix/rrflow offers) | the enterprise image (private registry) and the shippin.online deployment |

How they fit (the open-core pattern Mattermost and Sentry use):
- The public hub defines **extension points**: a registry where an extension adds routes, MCP
  tools, sign-in providers, audit sinks and edition storage. With none registered it is the
  complete self-host product; nothing in it reaches the network unless configured.
- The enterprise repo is a Cargo workspace that depends on the public crates **at a pinned tag**
  (git dependency), registers its extensions, and builds its own `devagotchi` binary and image.
  It never forks public code; a public change reaches enterprise by bumping the pin.
- CI: the public repo runs today's CI; the enterprise repo runs its own CI against the pinned
  public tag plus a nightly job against public `main` to catch breakage early.
- Secrets, the private registry and the enterprise deploys live only in the enterprise repo
  and Vaultix.

## Phases (each: spec → approval → one agent, one PR per item, merged on green)
- **E0 Org + repos + workspace** (first, because every later PR lands in the right place):
  Jesse creates the `devagotchi` GitHub org and transfers `EonsofStupid/devagotchi` into it
  (GitHub keeps redirects; CI secrets and the GHCR image name move with it — the image path
  changes to `ghcr.io/devagotchi/devagotchi`, so the hub deploy's `DEVAGOTCHI_TRACK` is updated
  in the same step). Then the agent: creates `devagotchi/devagotchi-enterprise` skeleton
  (workspace, pinned dependency, one example extension, CI, CLAUDE.md, the agent-board hooks),
  adds the extension registry to the public hub (empty by default), and updates the devspace
  (`deploy/devspace/`) to clone both. Needs: the org, and this session given access to the new
  repos.
- **E1 Surfaces:** host-based routing; `agenthub.<domain>` with its own MCP endpoint and
  resource metadata; audience-bound tokens; `agenthub_connect` + paste line; deprecate
  `board_*` on `/mcp`. DNS/Caddy for `agenthub.devagotchi.com` is Jesse's (or with his OK).
- **E2 Edition store + operator UI:** edition record, versions, publish/rollback, preview;
  site directory becomes defaults.
- **E3 Network config:** networks on an edition; offers and linking read from it.
- **E4 Second edition:** a shippin.online deployment from the same image (deploy files only).
- **E5 Licence switch:** LICENSE/NOTICE/Cargo metadata/headers per the decision above, before
  the repo goes public.

## Decisions to take (one at a time, before their phase)
1. E1: transition period for `board_*` on the main `/mcp` (proposal: 30 days, then removed).
2. E1: AgentHub's own UI home — `agenthub.<domain>/` shows the boards (proposal: yes, the phone
   board pages move there; `app.` keeps machines).
3. E2: who may edit an edition — the hub operator only (proposal) or delegated brand admins.
4. E5: BSL Additional Use Grant wording, Change Date (common: 4 years), and Change Licence.
