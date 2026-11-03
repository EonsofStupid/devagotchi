---
title: Privacy policy
updated: 1 November 2026
---
# Privacy policy

{product} at app.devagotchi.com and {domain} is run by {operator} ("we"). This page says
what the service keeps about you, why, and for how long.

## What we keep

- **Your account.** You sign in through Shippin (id.shippin.cloud). The hub receives your
  account ID and login name from it. So that your phone stays signed in, the hub keeps your
  sign-in session on its own disk, with the tokens from Shippin encrypted. The session ends
  when you sign out, after 30 days unused, after 90 days at most, or when Shippin stops
  accepting it.
- **A record of every tool call.** The time, your account ID, how you signed in, the tool,
  the machine, the job's ID and the outcome. It does not record the program's arguments or
  its output.
- **Your machines.** Each enrolled machine's name, the public key it signs in with, and when
  it was enrolled or revoked.
- **Boards.** The tickets and notes you and your agents write. A project's board belongs to
  your organization on Shippin: the other members of that organization, and their agents, see
  and work it. Nobody outside it does.

The output of a job passes through the hub to your AI app and is not stored by the hub. It
is kept by the broker on your own machine, under your control.

## What we do not do

No advertising, no selling or renting of data, no analytics or tracking scripts on these
pages, and no use of your data to train models.

## Who else sees it

The servers are ours, on hardware we rent. Your AI app (for example Claude) receives the
tool results you ask for, under its own privacy policy. We disclose data to anyone else only
when the law requires it.

## How long we keep it

The record of tool calls is append-only and kept as a security record while the service
runs. Machine records and boards are kept until you or your hub's operator delete them. Ask
us to delete your data and we remove everything that is not needed for that security record.

## Contact

Open an issue at [{link:issues}]({link:issues}), or see [Support](/support).
