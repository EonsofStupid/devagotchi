---
title: Privacy policy
updated: 3 November 2026
---
# Privacy policy

{product} at app.devagotchi.com and {domain} is run by {operator} ("we"). This page says
what the service keeps about you, why, and for how long.

## What we keep

- **Your account.** It is made the first time your AI app connects, so you can start
  without signing up. The hub keeps its ID, the name of the AI app that made it, when it was
  made and last used, its plan, and whether it is waiting on the early-access list. Until you
  secure it with a sign-in it is temporary, and it ends after 14 days unused.
- **How you sign in.** Whichever ways the hub offers: a passkey, Google, GitHub, a link sent
  to your email, or Shippin. For a passkey, the hub keeps its public key and never anything
  secret. For Google, GitHub or Shippin, the hub uses their answer once to learn your
  account ID there and your email address; it keeps those two and not their tokens. For an
  email link, it keeps your address; the link itself is kept only as a hash, works once and
  lasts 15 minutes. A hub that uses Shippin for every sign-in keeps Shippin's tokens,
  encrypted, with your session so it can renew it.
- **Your sign-in session.** So that your phone stays signed in, the hub keeps a session,
  stored only as a hash of the ID your browser holds. It ends when you sign out, after 30
  days unused, or after 90 days at most.
- **Your email address, when you give one.** It is used to send you sign-in links and to
  tell you when you are let in from the early-access list. Nothing else is sent to it.
- **Your network address, briefly.** To stop abuse, the hub limits how many accounts and
  sign-in emails one address can start. For that it keeps a keyed hash of the address, never
  the address itself, for one hour.
- **A record of every tool call.** The time, your account ID, how you signed in, the tool,
  the machine, the job's ID and the outcome. It does not record the program's arguments or
  its output.
- **A record of changes to your account.** When it was made, secured and let in, and any
  change to its plan, each with who made the change.
- **Your machines.** Each enrolled machine's name, the public key it signs in with, and when
  it was enrolled or revoked.
- **Phone notifications.** If you turn them on, the push address your browser gives the hub,
  used only to send you approvals.
- **Boards.** The tickets and notes you and your agents write. A board belongs to your
  account. If you sign in through an organization (for example on Shippin), it belongs to
  that organization instead: its other members, and their agents, see and work it. Nobody
  else does.

The output of a job passes through the hub to your AI app and is not stored by the hub. It
is kept by the broker on your own machine, under your control.

## What we do not do

No advertising, no selling or renting of data, no analytics or tracking scripts on these
pages, and no use of your data to train models.

## Who else sees it

The servers are ours, on hardware we rent. Your AI app (for example Claude) receives the
tool results you ask for, under its own privacy policy. If you sign in with Google, GitHub or
Shippin, that service knows you signed in here, under its own privacy policy. Emails go out
through the mail service we send with. We disclose data to anyone else only when the law
requires it.

## How long we keep it

The record of tool calls and the record of changes to your account are append-only, and are
kept as a security record while the service runs. A temporary account ends after 14 days
unused, and a sign-in session as described above. Machine records and boards are kept until
you or your hub's operator delete them. Ask us to delete your data and we remove everything
that is not needed for that security record.

## Contact

Open an issue at [{link:issues}]({link:issues}), or see [Support](/support).
