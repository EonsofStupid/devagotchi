# Spec: first connect, matched to the market leader step for step

Status: DRAFT for Jesse's approval. No agent starts until he approves this file.
Source of truth for the target flow: `docs/desktop-commander-teardown.md` §8 (checked live
2026-11-03). Rule: effortless first; nothing visible is added for security until real usage
is known. Anything not written here is out of scope.

## The flow a person gets (the whole thing)

| # | Desktop Commander (§8) | Devagotchi must do exactly this |
|---|---|---|
| 1 | "Add to Claude" from the directory | Person adds the connector `https://app.devagotchi.com/mcp` (directory listing is a later, separate task) |
| 2 | Sign-in page: "Continue with Google" / "Continue with email" (email + password, Sign In / Sign Up / Forgot password?) | The page the AI app opens shows the same two choices, nothing else on first view |
| 3 | AI gives one command | AI runs the one command itself if it has a shell on this computer; otherwise shows the one line to paste |
| 4 | Machine appears in the chat | `machines` shows it online; AI says so and carries on |

No other screen, prompt, nag or step anywhere in 1–4.

## Work items (each one PR, merged only when every check is green)

### A. Sign-in page (the page `/oauth/authorize` shows to someone with no session, hosted mode)
- Shows, in this order, only what is configured: **Continue with Google**, then a divider,
  then **email + password** with tabs **Sign in / Sign up** and a **Forgot password?** link.
  (GitHub and "Sign in with Shippin" buttons appear below Google only when configured.)
- Sign up: email + password (min 8 chars, nothing else). Account created with its UUID and
  tenant at once. Sign in: email + password. Wrong either way: "Email or password is wrong."
- After either, the browser goes straight back to the app: **no Connect/approve screen** for a
  person who just signed in from this request. (Already signed in + new app: one Connect
  screen, as today.)
- Forgot password: if outgoing email is configured, sends a reset link; if not, says
  "Password reset isn't available yet — sign in with Google."
- Passwords: argon2id, rate limit per address. Invisible to the person. No email verification
  required for anything. No passkey prompt anywhere in this flow.
- Anonymous "Connect makes an account" is removed from this page (config
  `tenancy.anonymous_connect`, default false).
- Existing accounts (the owner's passkey account, the anonymous ones) keep working.
- Operator: whoever signs in with Google as an email listed in `tenancy.operators`.
- Words from `content/messages.json`; defaults in `site/default/`.

### B. First machine = this computer
- `get-started` workflow = one step: "Connect this computer" → done. Phone step and "Keep your
  account" step removed from it.
- MCP instructions + `machine_add`/`workflow_start` descriptions: "If you can run commands on
  the user's computer (Claude Code, Codex CLI, a shell tool), run the one line yourself. Give
  the user a link only for a different machine." Never mention phone or approvals during setup.
- `/go/<token>` page: no "Keep your account", no secure/passkey/sign-in text. Opened on a
  phone: leads with "Open this on your computer" + Copy link + QR.
- Phone approvals: only offered when `approval_request` returns `phones: 0`.

### C. The four bugs from the live test (2026-11-04, dabo)
1. `offers` fails in Claude (output schema vs blank entries). Fix already pushed on branch
   `increment-78/offers-schema` — finish, test, merge.
2. Join script prints `tf9930eb1dd70-dabo`; must print the name `machines` shows (`dabo`).
3. Join script ends with developer text (MCP command, `tests\adapter_probe.py`). Replace with:
   name, "connected to <hub>", "starts with Windows / at boot", log path.
4. `run` on Windows returns PowerShell `#< CLIXML` progress junk. Broker starts PowerShell
   with progress silenced; Windows test asserts clean output.

### D. Owner setup (Jesse, not code)
- Google OAuth client: Web application, redirect
  `https://app.devagotchi.com/auth/provider/google/callback`; client id + secret into Vaultix;
  env names come from PR A's docs.

## Acceptance (the only definition of done)
1. Browser test, CI: AI client connects → sign-in page shows Google + email/password only →
   sign up with email/password → back in the app with no other screen → `whoami` shows the
   account → `machine_add` → join script runs → `machines` shows it online → `run` returns
   clean output. Fails on any extra screen.
2. Live, by Jesse from Claude: same path on app.devagotchi.com with his own PC; recorded in
   `verification/`.
3. `tests/live_check.py` against app.devagotchi.com passes the sign-in-page check.

## Order
C1 → A → B → C2–C4 → D (owner) → live acceptance.
