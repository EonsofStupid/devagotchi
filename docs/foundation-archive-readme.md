# Devagotchi source, audit, and growth plan

Devagotchi bridges AI and a person's workspace through a locally installed MCP client and a styled native desktop app. Windows, macOS and Linux are the product targets; the current execution provider and completed native UI checks are Linux only.

Start with `devagotchi/README.md` to build and run the working broker, MCP adapter and desktop inspector. `devagotchi/verification/README.md` explains the observed checks and their limits. The intended repository is https://github.com/eonsofstupid/devagotchi and the published site is https://devagotchi.justsayit.chatgpt.site; https://devagotchi.net is pending DNS validation. The GitHub connection returned 404, so this isolated source has not been reconciled with or pushed to that repository.

`desktop-bridge-audit-and-foundation.md` preserves the full reference audit and updates the architecture, confirmed choices, verification, acceptance criteria and next implementation steps. The `audit` directory contains original reproduction probes and captured results. `reference/DesktopCommanderMCP` is a partial, byte-verified source snapshot at commit f508e524469c26f6366e17dbfd3ee5a69d112127, with the upstream MIT license retained. It is not a full upstream checkout.

This package contains complete foundation source, pinned dependency manifests/lockfiles, generated contracts, app icons, build instructions, test harnesses and recorded evidence. Build products, installed dependencies, internal screenshots and test fixture contents are excluded. The new implementation is separate from the reference snapshot; the upstream MIT license is preserved with upstream material and does not select a license for the new project.

## Architecture and growth revision — October 17, 2026 UTC

The latest user direction requires extending existing responsibilities before adding boundaries, deliberate growth planning, and demonstrated performance and workstation experience improvements. The core revision extends its existing plan with task graphs, a trusted internal mesh, and recurring user-evidence requirements; desktop product source and historical raw reports remain unchanged. The website is a separately versioned implementation, published as Site version 3. Its receipt is included at devagotchi/verification/website-publication.json; its Git-backed source remains with the existing Site.

Read these documents before expanding the implementation:

- [Architecture and growth plan](devagotchi/docs/product-and-platform-plan.md): inspected scale limits, responsibility/reuse map, contract and migration decisions, native seams, phases, and traceability.
- [Performance and experience specification](devagotchi/docs/performance-and-experience-spec.md): reproducible workloads, metric definitions, proposed budgets, OS/user workflows, and honest comparison rules.
- [Standalone next execution guide](devagotchi/docs/implementation-guide.md): publication, G0 baseline, local graph, authenticated runtime, and two-broker mesh packets.
- [Task graph and mesh contract](devagotchi/docs/task-graph-and-mesh-contract.md): proposed graph records, attempts, routing, ownership, and distributed failure semantics.
- [Desktop Commander user evidence](devagotchi/docs/desktop-commander-user-evidence.md): recurring reports, corrections, resolution evidence, and planned acceptance cases.

The next packet is G0: establish a reproducible baseline before optimization or further features. No comparative performance measurements, new product tests, or Windows/macOS runtime results were produced in this planning revision. The plan's numbers are proposed acceptance budgets.

The pre-revision archive SHA-256 is 075a259e66f50168c84989d8cedb81c1debadd08e950bf2f3abb468c4e2b7987. Its original source manifest is preserved in audit/devagotchi-source-manifest-before-growth-plan.json. The planning-revision manifest records exact changed/new documentation and source preservation; devagotchi/verification/source-manifest.json inventories the current source/documentation package without claiming a new test run.

The verified development foundation is not yet a signed installer or a feature-complete Desktop Commander replacement. File/search/edit tools, PTY/ConPTY, workspace onboarding, native Windows/macOS providers, physical retention policy, complete audit history, named-client integration, signing/updates, runtime HTTP, graph execution, and mesh execution remain product work. The public website and reference service are published; custom-domain DNS validation and the stated connection/visual checks remain incomplete. Read the platform plan before making support claims.
