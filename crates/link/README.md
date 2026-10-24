# devagotchi link

The machine side of the link (plan N20, D3). It lets a Devagotchi hub reach the broker
on this machine without opening any port here: the link dials out to the hub over one
WebSocket on 443, proves this machine's key, and hands each routed request to the local
broker over private IPC. Jobs stay with the broker and outlive the link, the hub and the
client.

```text
AI client ──▶ hub (/mcp) ──call──▶ /link ◀──outbound WSS── devagotchi link ──IPC──▶ broker
```

## Enroll once, then run

On the hub host, issue a one-time code for the broker name (valid 15 minutes, one use):

```bash
devagotchi-hub enroll --state-dir ~/.local/state/devagotchi-hub --device gus
# dvg-3f0c…   (stdout, shown once)
```

The hub config lists the machine as a linked broker and grants it:

```json
"brokers": [ { "name": "gus", "link": true } ],
"grants":  [ { "subject": "<Nomen sub>", "brokers": ["gus"] } ]
```

On the machine, as the same user the broker runs as. The code goes in the environment (or
on stdin), never on the command line where other users could read it in the process list:

```bash
DEVAGOTCHI_ENROLL_CODE=dvg-3f0c… devagotchi-link enroll --state-dir ~/.local/state/devagotchi-link \
  --hub https://mcp.shippin.cloud --device gus --broker "<the broker's ready-line endpoint>"
devagotchi-link run --state-dir ~/.local/state/devagotchi-link
# devagotchi link ready: gus linked to https://mcp.shippin.cloud (broker …)
```

`run` reconnects by itself with backoff (1 s doubling to 60 s, up to 25% jitter) and beats
every 20 s; the hub drops a link silent for 60 s. It handles at most 16 calls at once and
answers `link_busy` beyond that. Start it at login (installer support is the next
increment). Exit code 2 means the hub refused this machine: a supervisor should not restart
it blindly. The hub tells a refused peer only `refused`; the reason (revoked, not enrolled,
wrong key, unknown name) is in the hub's audit log. A hub that briefly cannot check
(`unavailable`) is retried, not treated as a refusal.

## Security

| Property | How |
|---|---|
| No inbound port | The link only dials out. Nothing on the machine listens for the hub |
| The key never leaves | Ed25519 seed generated here into `device.key`. The state directory follows the broker's rule (Unix: 0700, ancestors real directories not writable by others; Windows: a protected user-only DACL, no link in its ancestry). The key is opened once without following links, that handle is checked (0600 and one link on Unix, user-only DACL on Windows) and read |
| No replay | The device signs `devagotchi-link/1`, the hub's public URL, the device name and a fresh 32-byte nonce per connection; the hub verifies strictly and refuses weak (small-order) keys |
| Enrollment | A one-time code bound to one device name; the hub stores only its SHA-256, spends it once, expires it after 15 minutes. A newer code supersedes older unused ones; binding a key or revoking cancels every unused code for that name |
| Revocation | `devagotchi hub revoke --device NAME` closes the link within 2 s and is checked again before each queued call is forwarded; new calls answer `device_revoked`; the link exits 2 |
| TLS | `https` hubs only, verified by the operating system's trust store; plain `ws` only for a loopback hub. Hub URLs are parsed: userinfo, queries and fragments are refused |
| Bounded hub side | `/link` takes at most 32 handshakes at once, a hello of at most 4 KiB within 10 s, 64 queued calls per device; every write to a device times out after 10 s; refusals are audited at most once a second with a count of the rest |
| The broker still decides | Each call carries one typed broker request; executable grants, workspace roots and deadlines are enforced by the broker as for a local caller |
| Audit | The hub records code issued and revoke (with the operator), enroll, connect and disconnect (with the key fingerprint, peer address and reason), refusals, and every routed call on its hash chain. The code itself is never logged |

## Check

`python3 tests/e2e.py --bin-dir target/debug` runs the whole life of a link against a real
broker and hub: not enrolled, a code that works once, calls through the link, offline when
it stops, back when it restarts, revoked and refused. CI runs it on Linux and both Windows
runners.
