# Security

## Reporting

Email security@shippin.cloud with the affected component (`devagotchi broker`, `devagotchi mcp`,
`devagotchi hub`, the inspector, the Shippin.Cloud deployment), the version or commit, steps
to reproduce, and what an attacker gains. Do not open a public issue for an exploitable
defect. Expect an acknowledgement within three days and a fix or a workaround with a date.

Pre-release: there is no backport branch. A fix lands on `main` and in the next tagged
release; the advisory names the first fixed version.

## What is in scope

- The broker's promises: a job belongs to the broker, not the client; only explicitly
  granted executables run; the workspace is the jail; output is bounded and journaled;
  recovery never replays an admitted job.
- The hub's promises: no call without a subject proven by the identity provider; no route
  without a grant; every routed call on the audit chain; the hub never admits a job.
- The adapter's promise: it never links the broker and never reaches the network.
- The supply chain: `Cargo.lock`, `deny.toml`, the pinned toolchain, attested release artifacts.

Out of scope: what a granted executable does with its own privileges once the broker ran
it as you; the security of the identity provider itself; the Shippin.Cloud control plane
(reported through the same address, handled under its own runbooks).

## The baseline

`docs/security-baseline.md` is the checklist every increment clears, with where each item
is enforced and how it is verified.
