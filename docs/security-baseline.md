# Security baseline

The minimum every increment clears before it merges, for the product (broker, adapter,
hub, inspector) and for the deployments that run it (shippin.cloud on Gold first, DevPulse
on the user's machine, a self-built system). One contract, two deployments: nothing below
depends on which one it is in. Plan references are to `product-and-platform-plan.md`
(S-constraints, decisions, gates) and §11a of the planning document.

Status: **enforced** = a check fails the build or the call; **built** = in the code and
verified by a receipt; **planned** = the increment that brings it.

## 1. Identity

| Rule | Mechanism | Where | Status |
|---|---|---|---|
| No call without a subject the identity provider proved; never a caller-supplied name | Bearer JWT verified against Nomen's JWKS: `iss`, `aud`, `exp`, `sub` required, RS/ES only; `whoami` returns the proven subject | `crates/hub/src/auth.rs` | built (H1) |
| Hosted clients find the issuer by standard, not by configuration | RFC 9728 protected-resource metadata; `401` with `resource_metadata`; Nomen dynamic client registration for claude.ai/ChatGPT, CIMD later | hub `/.well-known/oauth-protected-resource`; Nomen settings | built (hub) / planned (Nomen DCR, ticket) |
| People sign in with PKCE and MFA; agents on a laptop use device code; your own agents use service users with keys, never passwords | Nomen (Zitadel) flows; `shippin-bridge login`; service users with JWT profile | Nomen, `shippin-bridge`, hub audiences | built (Shippin N4) / planned (service users, H2) |
| Tokens are short-lived and audience-bound; refresh tokens rotate for public clients | Nomen token settings; hub rejects any `aud` not in its list | Nomen, hub config `audiences` | built (hub) / planned (Nomen policy) |
| A browser session outlasts a restart, never its person's account | session id only in a `__Host-` cookie, its SHA-256 on the hub; Nomen's tokens sealed (AES-256-GCM, key 0600, bound to the row) in a 0600 store; renewed with the rotated refresh token, one refresh at a time; ends on sign-out (refresh token revoked), 30 days unused, 90 days at most, or when Nomen refuses a refresh | hub `web.rs`, `web/sessions.rs` | built (18) |
| Opaque tokens are introspected, never trusted | RFC 7662 introspection with the hub's own credentials | hub | planned (H2) |
| An identity-provider outage neither signs anyone out nor lets anyone new in | A token Nomen confirmed keeps working while Nomen cannot be reached, until its own `exp` and at most 12 h after the confirmation (nobody can sign out of an unreachable Nomen either); a token Nomen never confirmed gets 503 with Retry-After, never 401; after a failure one request at a time tries Nomen again (2 s doubling to 30 s), 3 s connect / 5 s answer; start and end logged once; `/healthz` reports `issuer` | hub `nomen.rs`, `lib.rs` | built (27a) |

## 2. Authorization

| Rule | Mechanism | Where | Status |
|---|---|---|---|
| A project board belongs to the org the identity provider proved; another org never reaches it | Nomen's `urn:zitadel:iam:user:resourceowner:id` (asked for by every sign-in and in RFC 9728 `scopes_supported`); org boards under `org-<id>/` (0700); seats `<account>/<seat>` so members' agents never share a lease; the free limit per org | hub `auth.rs`, `nomen.rs`, `board/mcp.rs` | built (21a) |
| A subject reaches only the brokers it holds a grant for | `grants` by subject and broker name; `not_granted` otherwise | hub config, `Hub::route` | built (H1) |
| A broker runs only explicitly granted executables, exact arguments, inside the workspace (the default `granted` policy) | canonical executable grants, workspace jail, `executable_not_allowed` | `crates/core/src/broker.rs` | enforced (foundation tests) |
| Under the `account` policy (ruled 25 Oct 2026) a broker runs anything its OS account can, including a `command` line for the machine's shell, in any directory, with the account's environment; the account's permissions are the boundary, so a broker that takes commands runs as an unprivileged account and root goes only through an approved step | `exec_policy: account`, `command_not_allowed` under `granted` | `crates/core/src/broker.rs`, `execution/platform/*` | enforced; approvals for risky steps pending (box 2) |
| Identity and delegation never come from caller-controlled fields | subject from the token; dispatch envelope authenticated outside attribution fields | hub; mesh contract §6 | built (hub) / planned (M1) |
| Grants are data with an owner and a revocation path | hub config today; the agent-hub grant table with TTL and revoke later | hub; agent-hub | built / planned (H3, H4) |
| A machine an account adds is reachable by that account alone, and its code is a one-time, short-lived secret | "Add a machine" (signed in, same-origin form) mints `dvg-` + 128 random bits bound to the name and the account, valid 15 minutes, spent on first enrollment, superseded by a newer code, stored only as SHA-256, shown once in a `no-store` response; with `downloads`, the one-line join URL `/join/<code>` carries it (spent by enrolling, not by fetching; dead after use or 15 minutes; never logged by the hub, audited by machine name only); the machine generates its own Ed25519 key; other accounts and the hub owner get `unknown_broker`/`not_found` for its name; at most 16 machines per account; the owner's phones are told when it joins; Revoke ends its link within seconds | hub `devices.rs`, `link.rs`, `web/machines.rs`, `join.rs` | built (29a, 29b) |

## 3. Isolation

| Rule | Mechanism | Where | Status |
|---|---|---|---|
| One broker per workspace user; nothing runs as root | `install-workspace.sh` installs under the user's XDG dirs; `systemd --user`; no sudo anywhere in the product | `deploy/shippin/`, `deploy/devspace/` | built (PR #4, #6) |
| A tenant is a boundary VM; workspaces are containers inside it; projects are inner Incus projects | Shippin control plane (07-multi-tenancy) | Gold hostd | built (Shippin M1–M4) |
| Jobs are kernel-owned trees, not remembered PIDs | Job Objects with `KILL_ON_JOB_CLOSE`; cgroup v2 ladder on Linux | `crates/core/src/execution/platform/` | built (Windows) / planned (Linux ladder, Increment 2) |
| The hub and a local broker share one OS user; the IPC refuses other owners' sockets | `SO_PEERCRED`, owner-only socket, private directory | `crates/ipc/src/unix.rs` | enforced (security_boundary.py) |

## 4. Transport

| Rule | Mechanism | Where | Status |
|---|---|---|---|
| TLS terminates at the edge; the hub speaks HTTP on loopback or the tenant network only | Caddy with Let's Encrypt at `public_url`; hub `listen` on a private address; `install-devagotchi.sh --role hub --caddy` writes exactly that | Gold edge, hub config, `deploy/linux` | built (edge, installer; CI runs the hub behind Caddy) |
| DNS-rebinding and cross-origin calls are refused | rmcp `allowed_hosts` = public host + listen; origin allow-list when a browser UI exists | hub `main.rs` | built |
| A decision or a phone subscription comes only from the hub's own page | session cookie `SameSite=Lax`, plus `Sec-Fetch-Site: same-origin` (or a matching `Origin`) on `POST /approvals/decide` and `/push/subscribe`; a decision names the version shown | hub `web/approvals.rs`, `web/notify.rs` | built (10a, 10b) |
| Pages run no inline script; the approvals page loads one file from the hub | CSP `default-src 'none'` everywhere; the approvals page adds `script-src 'self'`, `worker-src 'self'`, `connect-src 'self'`, `manifest-src 'self'` for `/assets/push.js` and `/sw.js` | hub `web/pages.rs` | built (10b) |
| A machine installs only bytes the hub vouches for | the hub hashes every file `SHA256SUMS` lists at start and serves only those names from `/dist`; the join script carries each file's SHA-256 and refuses a mismatch before running anything; the files are the GitHub build the hub runs, checked with `gh attestation verify` at deploy | hub `downloads.rs`, `join.rs`; deploy | built (29b) |
| A push carries no command and no secret, and only the hub can send one | payload = title, link, id; RFC 8291 encryption per subscription; RFC 8292 VAPID key generated in the state directory (0600), public half only served | hub `push.rs` | built (10b) |
| Local IPC is a private socket or a named pipe with an owner-only DACL | Unix socket 0600 in a 0700 directory; `FIRST_PIPE_INSTANCE`, `REJECT_REMOTE_CLIENTS`, owner check | `crates/ipc` | enforced |
| No network code in `core` or `mcp`, ever | `cargo deny` bans every HTTP client crate except for the hub (`wrappers`) | `deny.toml`, `security.yml` | enforced (PR #8) |

## 5. Secrets

| Rule | Mechanism | Where | Status |
|---|---|---|---|
| Secrets live in Vaultix or the OS keychain, never in files, argv, logs, notes or git | `shippin-bridge` keychain; hub dev tokens only from a 0600 file and only in `dev` mode; config files 0600 | Shippin rules, hub | built |
| Nothing prints a token | hub logs code and message, never the bearer; broker journals never carry environment | hub, broker | built |
| Job environment is cleared and rebuilt from a declared profile | S10 today (cleared); environment-profile packet | broker | enforced / planned (G1) |

## 6. Audit

| Rule | Mechanism | Where | Status |
|---|---|---|---|
| Every routed call is one append-only, hash-chained row: when, who, how authenticated, what, where, outcome | SQLite chain; a call that cannot be recorded is refused; `/healthz` verifies | `crates/hub/src/audit.rs` | built (H1) |
| Every admitted job is journaled before effects and recoverable without replay | broker journal, idempotent admission, `unknown` after restart without a kernel object | `crates/core/src/journal.rs` | enforced (foundation tests) |
| The control plane's own audit is chained and operator-visible | Shippin `audit_log` trigger chain, Access & audit page | Gold | built (Shippin N2) |

## 7. Supply chain

| Rule | Mechanism | Where | Status |
|---|---|---|---|
| Dependencies are locked; vulnerable or yanked crates fail the build | `--locked` everywhere; `cargo deny check advisories` | CI, `security.yml` | enforced (PR #8) |
| Only permissive licenses are linked | `cargo deny check licenses` allow-list | `deny.toml` | enforced (PR #8) |
| Only crates.io; no git dependencies | `cargo deny check sources` | `deny.toml` | enforced (PR #8) |
| Release artifacts carry provenance; installers verify checksums and attestations | cargo-dist with GitHub attestations; `install-workspace.sh` checks sha256 and `gh attestation verify` | `release.yml`, `deploy/shippin/` | built (PR #5, #6) |
| The toolchain is pinned | `rust-toolchain.toml` (1.99.1), `rust-version` floor | repo | enforced |
| Every test binary runs on every OS in the matrix | nextest, 4-OS CI | `ci.yml`, `.config/nextest.toml` | enforced (PR #8) |
| GitHub Actions are pinned to commit SHAs, not moving tags | SHA pins in `ci.yml` and `security.yml` with the tag in a comment; Dependabot (`.github/dependabot.yml`) moves the pins and the lockfile weekly; `release.yml` is generated by dist and follows dist's pins | workflows | built (increment 8) |

## 8. Change control and recovery

| Rule | Mechanism | Where | Status |
|---|---|---|---|
| One concern per PR, verified and recorded | PR body states what was verified and where; `verification/` receipts | repo | enforced by review |
| A change on Gold goes through a ticket with a rollback and a 60 s verify | gold-policy, `gold-verify` 7/7, release dirs and rollback scripts | Gold | built (Shippin guardrails) |
| State survives restart and rebuild | journal and config in the user's home; `/devspace` volume; Incus snapshots daily, 14 kept | `deploy/devspace/`, Gold | built |
| A restore is drilled, not assumed | monthly restore drill of a tenant volume and the control-plane database | Gold | planned (Shippin N7) |

## What a reviewer checks on every PR

1. No new network dependency outside the hub (`cargo deny` says so).
2. No secret, token, key or password in code, config samples, tests, logs or the PR text.
3. Every new call path names its subject and its grant check, and lands on the audit chain.
4. Every new binary or service is installed as a user, with a unit that restarts it, and a rollback.
5. The receipt exists and says what was not verified.
