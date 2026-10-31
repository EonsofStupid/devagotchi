# Devagotchi website and public reference service

Product site for the Devagotchi AI-to-workspace bridge. The current site provides public product documentation, a Linux source preview, a read-only HTTP reference API, and a stateless MCP reference endpoint. Workstation execution remains in the local broker; this Worker never dispatches commands to a desktop.

## Routes

| Route | Purpose |
| --- | --- |
| `/` | Product overview and actual Linux desktop screenshot |
| `/connect` | Public API/MCP reference and local broker quickstart |
| `/architecture` | Proposed task graph and internal mesh contracts |
| `/research` | Qualified primary-source evidence and acceptance cases |
| `/api/v1/capabilities` | Machine-readable availability and connection metadata |
| `/api/v1/reference?topic=local-tools` | Versioned static reference topics |
| `/openapi.json` | OpenAPI 3.1 description of the two public GET routes |
| `/mcp` | MCP 2026-07-28 reference tools; POST only |
| `/llms.txt` | Short index for agents |

Profiles on this site (`profiles`, unique email, today filled from a ChatGPT session) are not Devagotchi accounts. Signing in through the hub's authorization server returns a Devagotchi account id in the access token `sub`. `lib/hub-account.ts` links that id onto the profile when the token's email is verified and matches the profile, and refuses an unverified or different address. An id already stored on the profile is kept.

The MCP catalog contains only `devagotchi_get_capabilities` and `devagotchi_get_reference`. Sites owns hosted MCP authentication and viewer access checks. The application independently validates protocol metadata, HTTP headers, Origin, tool arguments, and a bounded request body. Neither reference handler accesses the existing database or object storage.

## Development and verification

Use Node.js 22.13 or newer and the committed lockfile. Dependencies and the React Aria / Style Dictionary design foundations extend the existing site.

```bash
npm ci
npm test
npm run typecheck
npm run build
```

`npm test` runs the focused protocol and reference-route checks. A production build is a separate gate; a passing handler test does not establish deployed authentication or named-client compatibility. The obsolete starter loading-skeleton test has been replaced by the current reference-service checks.

For managed Sites work, use the installed Sites dependency, build, source-sync, and preview helpers. The supervised preview is the sole development server. Source publication must preserve the project ID in `.openai/hosting.json` and the existing public audience. Credentials belong in helper stdin, never source files or command arguments.

## Content ownership

`lib/devagotchi-reference.ts` owns the reference version, platform availability, endpoint descriptions, copyable quickstart, and illustrative graphs. All pages and HTTP/MCP responses reuse it. `public/reference/broker-contracts.schema.json` is a byte-for-byte copy of the generated Rust broker contract from the source preview; changes should be regenerated at that owner.

The source ZIP is a deliberately scoped Linux development preview. Its checksum accompanies the download. No signed installers, macOS/Windows execution providers, graph executor, remote workstation HTTP service, or mesh scheduler are claimed. The project license is not yet selected.

The deployment origin is the published Sites hostname until the requested `devagotchi.net` binding is active. Update the single `SITE_ORIGIN` and the `llms.txt` links after domain activation; do not advertise an unverified connection URL.

## Retained data and routes

This project reuses the existing Devagotchi Site identity. Its earlier `/sanctuary` experience, authenticated pet routes, database schema/migrations, and `DB` / `PET_ASSETS` bindings remain intact. Public bridge-reference routes are independent of those records. Routine publication must not delete or recreate these resources.

## Architecture

The product core has its own canonical broker and product plan. The website is an adapter for documentation and public reference access. Proposed task graphs and mesh dispatch belong to extensions of the existing broker, operation journal, policy, and platform providers. This website must not become a second command-execution owner.
