# devagotchi-link

The machine side of the link (plan N20, D3). It lets a Devagotchi hub reach the broker
on this machine without opening any port here: the link dials out to the hub over one
WebSocket on 443, proves this machine's key, and hands each routed request to the local
broker over private IPC. Jobs stay with the broker and outlive the link, the hub and the
client.

```text
AI client ──▶ hub (/mcp) ──call──▶ /link ◀──outbound WSS── devagotchi-link ──IPC──▶ bridge-broker
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

On the machine, as the same user the broker runs as:

```bash
devagotchi-link enroll --state-dir ~/.local/state/devagotchi-link \
  --hub https://mcp.shippin.cloud --device gus \
  --broker "<the broker's ready-line endpoint>" --code dvg-3f0c…
devagotchi-link run --state-dir ~/.local/state/devagotchi-link
# devagotchi-link ready: gus linked to https://mcp.shippin.cloud (broker …)
```

`run` reconnects by itself with backoff (1 s doubling to 60 s, up to 25% jitter) and beats
every 20 s; the hub drops a link silent for 60 s. Start it at login (installer support is
the next increment). Exit code 2 means the hub refused this machine (revoked, not enrolled,
wrong key, unknown name): a supervisor should not restart it blindly.

## Security

| Property | How |
|---|---|
| No inbound port | The link only dials out. Nothing on the machine listens for the hub |
| The key never leaves | Ed25519 seed generated here into `device.key`; the state directory is 0700 and the key 0600 on Unix, a protected DACL for the user on Windows, checked on every start |
| No replay | The device signs `devagotchi-link/1`, the hub's public URL, the device name and a fresh 32-byte nonce per connection |
| Enrollment | A one-time code bound to one device name; the hub stores only its SHA-256, spends it once, expires it after 15 minutes |
| Revocation | `devagotchi-hub revoke --device NAME` closes the link within 2 s; new calls answer `device_revoked`; the link exits 2 |
| TLS | `https` hubs only, verified by the operating system's trust store; plain `ws` is accepted only for a loopback hub |
| The broker still decides | Each call carries one typed broker request; executable grants, workspace roots and deadlines are enforced by the broker as for a local caller |
| Audit | The hub records enroll, connect, disconnect (with the reason) and every routed call on its hash chain |

## Check

`python3 tests/e2e.py --bin-dir target/debug` runs the whole life of a link against a real
broker and hub: not enrolled, a code that works once, calls through the link, offline when
it stops, back when it restarts, revoked and refused. CI runs it on Linux and both Windows
runners.
