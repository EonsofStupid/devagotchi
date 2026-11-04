# Spec: first connect (sign-in → this computer → machine online)

Status: APPROVED by Jesse 2026-11-04. One agent executes it in order.
Target flow: `docs/desktop-commander-teardown.md` §8. Rule: effortless first; nothing visible
is added for security. Anything not written here is out of scope. Paths are relative to
`crates/hub/` unless they start with `crates/`, `deploy/` or `tests/`. Line numbers are from
`main` at 2026-11-03 and are hints only; the function names are the anchors.

## The whole flow a person gets

| # | What the person sees | What makes it happen |
|---|---|---|
| 1 | Adds connector `https://app.devagotchi.com/mcp` in their AI app | unchanged |
| 2 | One page: **Continue with Google**, "or", email + password (**Sign in / Sign up**, **Forgot password?**) | PR A2 |
| 3 | Straight back to the AI app. No Connect screen, no passkey, no "secure your account" | PR A2 |
| 4 | AI runs the one line itself (it has a shell), or shows one line to paste | PR B |
| 5 | AI says "`<name>` is online" and carries on | PR B, C2–C4 |

Fails the spec: any extra screen, prompt or nag in 1–5.

## Modules (each owns one thing; nothing else changes)

| Module | Kind | Owns | Public surface |
|---|---|---|---|
| `src/accounts/passwords.rs` | new | password hashes, sign-up, sign-in, set | `Accounts::{sign_up_password, sign_in_password, set_password}` |
| `migrations/{postgres,sqlite}/0013_passwords.sql` | new | table `passwords` | — |
| `src/web/sign_in.rs` | new | the sign-in page HTML + its two POSTs | `page()`, `routes()` |
| `src/web/session.rs` | new (extracted) | open a session + cookie + redirect | `open_and_return()` |
| `src/web/oauth.rs` | changed | redirect to sign-in; skip Connect after fresh sign-in | `authorize`, `decide`, new `approve` |
| `src/config/tenancy.rs` | changed | flag `anonymous_connect` | field |
| `src/site/model.rs` `Messages` | changed | copy struct `sign_in` | field |
| `site/default/content/messages.json`, `workflows.json`, `site.json` | changed | all words | — |
| `src/join.rs`, `src/link.rs` `join_script`, `src/join/join.{sh,ps1}`, `deploy/windows/install-devagotchi.ps1` | changed | join output | — |
| `crates/core/src/execution/platform/windows.rs` `shell_command` | changed | clean PowerShell output | — |

No new crate, process or service. One new dependency: `argon2` (RustCrypto, MIT/Apache-2.0)
in `crates/hub/Cargo.toml`; `cargo deny --all-features check` must pass.

## PR C1 — `offers` works in Claude (branch `increment-78/offers-schema`, exists)
- Already pushed: `src/output_schema.rs`, `src/site/offers.rs`, `src/web/landing/network.rs`,
  `src/tenant_tests.rs` (b3a5355; autosave 553eafc has the rest — take the autosave).
- Finish: blank offerings (no description/url) are left out of `offers` output, not sent
  half-empty. Test in `src/offers_tests.rs`: with one blank offering configured, the
  `offers` result validates against the tool's `outputSchema` (use the validator in
  `output_schema.rs`).
- Done when: the test passes; full CI green.

## PR A1 — password accounts (library only, no web)
1. `migrations/postgres/0013_passwords.sql` and `migrations/sqlite/0013_passwords.sql`:
   `passwords(account_id TEXT PRIMARY KEY REFERENCES accounts, email TEXT UNIQUE NOT NULL,
   hash TEXT NOT NULL, created_at BIGINT NOT NULL, updated_at BIGINT NOT NULL)`. Email stored
   trimmed + lowercased. Copy the style of `0012_offers.sql` exactly (both dialects).
2. `src/accounts/passwords.rs` (≤150 lines), `impl Accounts`:
   - `sign_up_password(email, password) -> BridgeResult<Account>`: reject `password.len() < 8`
     (`code = "password_short"`); reject an existing email (`code = "email_taken"`). Else
     create the account and tenant exactly as an identity sign-in does — call
     `sign_in_identity(&Proven { provider: "email", subject: email, verified_email: None,
     name: None })` (identities.rs) — then insert the argon2id hash. One transaction if the
     store allows; otherwise insert the hash second and delete the account on failure.
   - `sign_in_password(email, password) -> BridgeResult<Account>`: unknown email or wrong
     password both return `code = "password_wrong"` (same message, same timing: hash a dummy
     on unknown email). Disabled account → `account_disabled` (existing code).
   - `set_password(account, password)`: upsert; used by Forgot password (A2).
   - Limit: 10 failed sign-ins per email per 15 minutes, in memory (a `Mutex<HashMap>` on
     `Accounts`, same pattern as `starts_per_hour` in `src/web/hosted.rs`) → `code = "slow_down"`.
   - argon2 `Argon2::default()` (argon2id, crate defaults). Never log the password or hash.
3. Wire the module in `src/accounts.rs` (`mod passwords;`). Nothing else.
4. Tests in `src/accounts/tests.rs`: sign up → sign in works; wrong password and unknown
   email give the same error; short password refused; duplicate email refused; the account
   has a `t-<uuid>` tenant and is not provisional (`provisional_until` is `None`, so
   `whoami.secure_url` is `None`).

## PR A2 — the sign-in page and no Connect screen after it
1. `src/config/tenancy.rs` `TenancyConfig`: add `pub anonymous_connect: bool`, default
   `false`, `#[serde(default)]`. Document in the config docs next to `max_unsecured`.
2. `src/web/session.rs` (new): move the tail of `providers::finish` (from `// Still the same
   account` to the end) into `pub(super) async fn open_and_return(hub, current, account,
   return_to, extra_cookies) -> Response`. `providers::finish` calls it. No behaviour change.
3. `src/web/sign_in.rs` (new, ≤150 lines):
   - `pub fn page(hub, return_to, tab: Tab, notice: Option<&str>) -> Response`:
     `hub.providers.buttons(&site, return_to, false)` first (Google, then GitHub/Shippin only
     if configured), then `messages.sign_in.or`, then one `<form method=post>` with email +
     password and two tab links (`?tab=in` / `?tab=up`, plain links, no JS needed), submit
     posting to `/auth/password` or `/auth/password/new`, then the Forgot link
     `/auth/password/forgot?return_to=…`. Same `REFERRER_POLICY same-origin` as
     `passkeys::login_page`. No passkey text, no "secure", no anonymous note.
   - `POST /auth/password` → `sign_in_password`; `POST /auth/password/new` →
     `sign_up_password`. Success → `session::open_and_return(…, return_to, [fresh cookie])`.
     Error → `page()` again with the matching `messages.sign_in.*` notice, status 400/429.
   - Fresh cookie: `dvg_fresh=<sha256(return_to)>`, HttpOnly, SameSite=Lax, Max-Age 300.
   - `GET/POST /auth/password/forgot`: if `hub.providers.email.is_some()`, send the existing
     email sign-in link (`email::ask`) with `return_to=/account#password`, and show
     `messages.sign_in.reset_sent`; else show `messages.sign_in.reset_unavailable`
     ("Password reset isn't available yet — sign in with Google."). The account page gets a
     "Set a password" form posting to `set_password` (in `routes.rs` account page, the
     sign-ins list).
   - Google callback must also set the fresh cookie: `providers::callback` passes it into
     `finish` → `open_and_return`.
   - Same email, Google later (Jesse, 2026-11-04: "UI popup requests that you link the
     account"): in `providers::finish`, when the Google identity is unknown, its
     `verified_email` matches a `passwords.email`, and nobody is signed in, do not create an
     account. Show one page: "`<email>` already has an account. Link Google to it?" with
     **Link** and **Keep separate** (creates the new account as today). **Link** carries one
     password field ("Password for `<email>`"): the POST checks it with
     `sign_in_password(email, password)` (same per-email `slow_down` limit) and only then does
     `link_identity(account, proven)` then `open_and_return` (Jesse, 2026-11-04: otherwise
     someone who signed up first with another person's email would share their account). A
     wrong password shows the page again with `messages.sign_in.wrong`, status 400, still with
     **Keep separate** and **Forgot password?**. The proven identity rides in a signed,
     HttpOnly, 5-minute cookie between the requests. Copy keys `sign_in.link_title`,
     `sign_in.link`, `sign_in.keep_separate`, `sign_in.link_password` ("Enter this account's
     password to link Google to it."). Tests: right password links; wrong password links
     nothing and leaves no session; Keep separate.
   - `routes()` merged where `email::routes` / provider routes are merged in `src/lib.rs`.
4. `src/web/routes.rs` `login()` (~272): when `config.multi_tenant()`, return
   `sign_in::page(...)`. Single-tenant (owner's own hub) keeps `passkeys::login`.
5. `src/web/oauth.rs`:
   - `authorize()` (~363): the `Ok(None) if multi_tenant()` arm becomes
     `Ok(None) if multi_tenant() && tenancy.anonymous_connect`; otherwise the existing
     `see_other(&sign_in)` runs (it already carries `return_to` = this request).
   - Extract the code-issuing block of `decide()` (the `issue_code` call through the redirect
     with `code` + `state`) into `async fn approve(state, hub, person, checked) -> Response`.
   - `authorize()`: when a person is signed in AND the request's `dvg_fresh` cookie equals
     `sha256(this_request(&query))`, call `approve()` directly and clear the cookie. Else
     render the Connect page as today (already-signed-in person + new app = one screen, kept).
   - `decide()`: the `hosted::start` branch runs only when `anonymous_connect` is true.
6. Copy: `src/site/model.rs` `Messages` gets `pub sign_in: SignInMessages { title, or,
   tab_in, tab_up, email, password, submit_in, submit_up, forgot, wrong, taken, short,
   slow_down, reset_sent, reset_unavailable, set_password }`, `#[serde(default)]` with these
   defaults in `site/default/content/messages.json`:
   "Sign in", "or", "Sign in", "Sign up", "Email", "Password", "Sign in", "Create account",
   "Forgot password?", "Email or password is wrong.", "That email already has an account. Sign in instead.",
   "Use at least 8 characters.", "Too many tries. Wait a few minutes.",
   "Check your email for a sign-in link.", "Password reset isn't available yet — sign in with Google.",
   "Set a password".
7. Existing accounts keep working: anonymous ones (e.g. `dabo`'s) still sign in by their
   cookie; the owner keeps the passkey page on single-tenant hubs; `email`-link sign-in stays.
8. Tests (`src/web/tests.rs`): multi-tenant, no session → `/oauth/authorize` 303s to
   `/auth/login?return_to=…`; that page contains the Google link (with a dummy provider
   configured), both forms, "Forgot password?", and none of: "passkey", "Secure", "Connect";
   sign up POST → 303 to the authorize URL with `dvg_fresh` set → that GET 303s straight to
   `redirect_uri?code=…&state=…`; a second app later shows the Connect page;
   `anonymous_connect=true` restores today's behaviour (existing hosted tests pass unchanged
   with the flag set in their fixture).

## PR B — first machine is this computer
1. `site/default/content/workflows.json` `get-started`: steps = `machine`, `done` only.
   Intro: "Connect this computer. One click and one pasted line; your AI watches and carries
   on when it's done." Step text: "Pick what this computer runs and press the button. You get
   one line to paste here: it installs {product}, joins your account and starts with the
   computer." Update the asserts in `src/workflow_tests.rs` (~226, ~658) to
   `["machine", "done"]`. `phone-approvals` workflow stays as its own workflow.
2. `site/default/site.json` `mcp.instructions`: replace the sentence starting "To connect a new
   machine" with: "To connect a machine: if you can run commands on the user's computer
   (Claude Code, Codex, a shell tool), call machine_add and run its line for that OS
   yourself. Otherwise call workflow_start with get-started and give the user its one link;
   then workflow_wait. Do not bring up phones or approvals while connecting." Remove the
   `secure_url` sentence.
3. `src/hub.rs` `machine_add` description (~492): append "If you can run commands on this
   computer, run the command for its OS yourself; give the user a command only for another
   machine." `workflow_start` description: same sentence in reverse ("use this when you
   cannot run commands on the user's computer").
4. `src/web/workflow.rs` `/go/<token>` page: when the User-Agent is a phone (`Mobi|Android|
   iPhone`), put first "Open this on your computer" + a Copy link button + the QR the page
   already draws for the phone step. Copy key `messages.workflow.open_on_computer`.
5. Tests: `src/workflow_tests.rs` get-started has 2 steps; `tests/workflow_browser.py` adds a
   phone-UA page load asserting "Open this on your computer" is first and no "Keep your
   account" anywhere.

## PR C2+C3 — join prints the right name and only user words
1. `src/join.rs` replacements: add `("@NAME@", shown)` where `shown` is the name `machines`
   shows. `src/link.rs` `join_script` takes it from `tenant_machines::shown_name(subject,
   device)` and passes it in. `unfilled()` in the join tests includes `@NAME@`.
2. `src/join/join.sh` line ~35 and `src/join/join.ps1` line ~45: print
   `done: @NAME@ is connected to $hub and starts with the computer. Tell your AI to use "@NAME@".`
   `--device` keeps the stored name.
3. `deploy/windows/install-devagotchi.ps1` lines ~374–376 (MCP command, Claude Code line,
   `adapter_probe.py`): print only when `-Developer` is passed. `join.ps1` never passes it.
   Same check for `deploy/linux/install-devagotchi.sh` if it prints developer lines.
   Add the log path line: "Log: <path>".
4. Tests: join tests assert the rendered script contains `dabo`-style shown name and not the
   `t…-` tag; `tests/e2e.py` Windows path asserts no "adapter_probe" in installer output.

## PR C4 — clean `run` output on Windows
1. `crates/core/src/execution/platform/windows.rs` `shell_command`: encode
   `"$ProgressPreference='SilentlyContinue';" + command` instead of `command`.
2. Real-OS test in the same file's tests (Windows CI runners): command
   `Write-Progress -Activity x -Status y; 'ok'` → stdout trimmed == `ok`, stderr does not
   contain `#< CLIXML`.

## D — owner setup (Jesse, not code)
Google OAuth client, type Web application, redirect
`https://app.devagotchi.com/auth/provider/google/callback`; id + secret into Vaultix under the
env names PR A2's docs give. Until then the page shows email + password only.

## Acceptance (the only definition of done)
1. `tests/first_connect_browser.py` (new, Playwright, same harness as
   `tests/workflow_browser.py`; added to the CI job that runs it): multi-tenant hub → DCR
   client → open authorize URL → page has the two choices only → sign up → the very next
   navigation is the client's `redirect_uri` with a code (count navigations: exactly 2 hub
   pages) → token → MCP `whoami` (no `secure_url`) → `machine_add` → run the Linux line →
   `machines` shows it online under its shown name → `run echo ok` returns `ok`.
2. Live, Jesse from Claude on app.devagotchi.com with his PC; receipt in `verification/`.
3. `tests/live_check.py` gains a check: `/auth/login` on the live hub contains "Sign up" and
   no "passkey".

## Order and size
C1 → A1 → A2 → B → C2+C3 → C4 → D → live acceptance. Six PRs, one agent, sequential, each
merged only on full green before the next starts. Push every 15 minutes.

## Not in this spec
Phone push, Shippin linking, Mac build, directory listing, email verification, passkeys,
merging accounts other than the link page in A2.
