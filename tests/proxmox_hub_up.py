#!/usr/bin/env python3
"""deploy/proxmox/hub-up.sh against a stand-in Proxmox host.

The script only talks to Proxmox through `pct` and to the network through `curl`, so both
are replaced by stubs that record what they were asked and answer like a host would: a
container's config, `pct exec` inside it, `pct push`, GitHub's API and raw files (served
from this checkout), and the hub's /healthz inside the container and at the public name.
What runs inside the container (deploy/compose/install.sh) is checked for real by
tests/deploy_scripts.sh; this checks the orchestration around it.

    python3 tests/proxmox_hub_up.py
"""

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "deploy" / "proxmox" / "hub-up.sh"
DOMAIN = "app.example.com"
NEW = "a" * 64
OLD = "b" * 64
SHA = "0123456789abcdef0123456789abcdef01234567"

PCT = r'''#!/usr/bin/env python3
import json, os, shutil, sys
fake = os.environ["FAKE"]
state_file = os.path.join(fake, "state.json")
state = json.load(open(state_file))
args = sys.argv[1:]
state["calls"].append(args)
def save():
    json.dump(state, open(state_file, "w"))
cmd, ct = args[0], args[1] if len(args) > 1 else ""
if ct != state["ct"]:
    save(); sys.exit(2)
if cmd == "config":
    print(state["config"]); save(); sys.exit(0)
if cmd == "snapshot":
    save(); sys.exit(1 if state.get("no_snapshot") else 0)
if cmd == "set":
    features = args[args.index("--features") + 1]
    lines = [l for l in state["config"].splitlines() if not l.startswith("features:")]
    state["config"] = "\n".join(lines + ["features: " + features]); save(); sys.exit(0)
if cmd == "status":
    print("status: running"); save(); sys.exit(0)
if cmd in ("reboot", "start"):
    save(); sys.exit(0)
if cmd == "push":
    shutil.copy(args[2], os.path.join(fake, "pushed", os.path.basename(args[3])))
    save(); sys.exit(0)
if cmd == "exec":
    inner = args[args.index("--") + 1:]
    line = " ".join(inner)
    save()
    if "getent hosts" in line: sys.exit(0)
    if "systemctl --user disable --now devagotchi-hub" in line:
        state["stopped"] = True; save(); sys.exit(0)
    if "ss -Hltn" in line: sys.exit(0 if state.get("port_busy") else 1)
    if "ss -Hltnp" in line: print("LISTEN 0 4096 127.0.0.1:8787 users:((\"devagotchi\"))"); sys.exit(0)
    if "command -v caddy" in line: sys.exit(0 if state.get("edge") else 1)
    if "command -v curl" in line or inner[0] == "mkdir": sys.exit(0)
    if inner[0].endswith("/install.sh"):
        state["install"] = inner[1:]; save(); sys.exit(0)
    if inner[0] == "curl" and "/healthz" in line:
        print(json.dumps({"audit": {"entries": 1, "head": state["new"], "ok": True}, "ok": True},
                         separators=(",", ":"))); sys.exit(0)
    if "docker compose logs" in line:
        print("hub-1  | devagotchi hub is unclaimed: nobody can sign in as its owner yet. Claim it in a browser before 2026-11-03 00:00 UTC:")
        print("hub-1  |   https://" + state["domain"] + "/claim#ABCD-EFGH-JKMN-PQRS-TVWX-YZ23"); sys.exit(0)
    print("unexpected pct exec: " + line, file=sys.stderr); sys.exit(97)
print("unexpected pct " + " ".join(args), file=sys.stderr); sys.exit(98)
'''

CURL = r'''#!/usr/bin/env python3
import json, os, shutil, sys
fake = os.environ["FAKE"]
state = json.load(open(os.path.join(fake, "state.json")))
args = sys.argv[1:]
url = [a for a in args if a.startswith("http")][-1]
out = args[args.index("-o") + 1] if "-o" in args else None
if url.startswith("https://api.github.com/repos/"):
    print('{\n  "sha": "' + state["sha"] + '",\n  "node_id": "x"\n}'); sys.exit(0)
if url.startswith("https://raw.githubusercontent.com/"):
    assert "/" + state["sha"] + "/deploy/compose/" in url, url
    shutil.copy(os.path.join(state["root"], "deploy", "compose", url.rsplit("/", 1)[1]), out); sys.exit(0)
if url == "https://" + state["domain"] + "/healthz":
    head = state["old"] if state.get("public_old") else state["new"]
    print(json.dumps({"audit": {"entries": 9, "head": head, "ok": True}, "ok": True},
                     separators=(",", ":"))); sys.exit(0)
print("unexpected curl " + " ".join(args), file=sys.stderr); sys.exit(22)
'''


def run(case: str, *, config: str, extra=(), **state) -> tuple[int, str, dict]:
    with tempfile.TemporaryDirectory(prefix="hub-up-") as tmp:
        fake = Path(tmp)
        (fake / "bin").mkdir()
        (fake / "pushed").mkdir()
        for name, body in (("pct", PCT), ("curl", CURL), ("sleep", "#!/bin/sh\nexit 0\n")):
            path = fake / "bin" / name
            path.write_text(body)
            path.chmod(0o755)
        (fake / "state.json").write_text(json.dumps({
            "calls": [], "ct": "110", "config": config, "domain": DOMAIN, "new": NEW,
            "old": OLD, "sha": SHA, "root": str(ROOT), **state,
        }))
        env = dict(os.environ, FAKE=str(fake), PATH=f"{fake / 'bin'}:{os.environ['PATH']}",
                   DEVAGOTCHI_ALLOW_NONROOT="1", DEVAGOTCHI_HUB_UP_LOG=str(fake / "log"))
        proc = subprocess.run(
            ["bash", str(SCRIPT), "--ct", "110", "--domain", DOMAIN, *extra],
            env=env, capture_output=True, text=True, timeout=60,
        )
        result = json.loads((fake / "state.json").read_text())
        result["pushed"] = sorted(p.name for p in (fake / "pushed").iterdir())
        for name in result["pushed"]:
            same = (fake / "pushed" / name).read_bytes() == (ROOT / "deploy/compose" / name).read_bytes()
            assert same, f"{case}: pushed {name} is not this checkout's"
        output = proc.stdout + proc.stderr
        return proc.returncode, output, result


UNPRIVILEGED = "arch: amd64\nhostname: hub\nnet0: name=eth0,bridge=vmbr1,ip=10.0.0.110/24,gw=10.0.0.1\nrootfs: local-lvm:vm-110-disk-0,size=8G\nunprivileged: 1"
PRIVILEGED = "arch: amd64\nfeatures: nesting=1\nhostname: hub\nnet0: name=eth0,bridge=vmbr1,ip=dhcp\nrootfs: local:110/vm-110-disk-0.raw,size=8G"


def check(case, condition, output):
    if not condition:
        print(f"FAIL {case}\n{output}")
        sys.exit(1)


def main() -> int:
    # 1. An unprivileged container whose Caddy serves the domain: snapshot, Docker's
    #    features turned on with a restart, the old hub off, the hub behind that Caddy on
    #    loopback, the public name checked, the claim link printed.
    code, out, s = run("edge", config=UNPRIVILEGED, edge=True, extra=["--rp-id", "example.com"])
    calls = [c[0] for c in s["calls"]]
    check("edge", code == 0, out)
    check("edge: snapshot first", calls[1] == "snapshot", out)
    check("edge: features", ["set", "110", "--features", "nesting=1,keyctl=1"] in s["calls"], out)
    check("edge: restarted for them", "reboot" in calls, out)
    check("edge: old hub stopped", s.get("stopped"), out)
    check("edge: behind the existing Caddy", s["install"][s["install"].index("--edge") + 1] == "external"
          and s["install"][s["install"].index("--hub-bind") + 1] == "127.0.0.1"
          and s["install"][s["install"].index("--rp-id") + 1] == "example.com"
          and s["install"][s["install"].index("--domain") + 1] == DOMAIN, out)
    check("edge: the deploy files of one commit", s["pushed"] == sorted(
        ["install.sh", "compose.yaml", "update.sh", "backup.sh"]), out)
    check("edge: claim link", f"https://{DOMAIN}/claim#ABCD-EFGH-JKMN-PQRS-TVWX-YZ23" in out
          and "Claim it now" in out, out)
    print("PASS an unprivileged container behind its own Caddy is reloaded as the hub")

    # 2. A privileged container with nesting already on and no Caddy: no restart, no
    #    keyctl, the stack's own Caddy; no snapshot when the storage takes none.
    code, out, s = run("own caddy", config=PRIVILEGED, no_snapshot=True)
    calls = [c[0] for c in s["calls"]]
    check("own caddy", code == 0, out)
    check("own caddy: no restart", "set" not in calls and "reboot" not in calls, out)
    check("own caddy: stack Caddy", s["install"][s["install"].index("--edge") + 1] == "caddy", out)
    check("own caddy: says no snapshot", "takes no snapshots" in out, out)
    print("PASS a privileged container with no Caddy gets the stack's own")

    # 3. The public name still answers from somewhere else: it says so and fails.
    code, out, s = run("public", config=UNPRIVILEGED, edge=True, public_old=True)
    check("public", code == 1 and "does not answer from the new hub" in out, out)
    print("PASS a public name that does not reach the new hub is a failure, said plainly")

    # 4. Something else holds the hub's port after the old hub is off: nothing installed.
    code, out, s = run("port", config=UNPRIVILEGED, edge=True, port_busy=True)
    check("port", code == 1 and "still listens on port 8787" in out and "install" not in s, out)
    print("PASS a busy port stops it before anything is installed")

    # 5. No such container.
    code, out, s = run("missing", config=UNPRIVILEGED, ct="111")
    check("missing", code == 1 and "no container 110" in out and len(s["calls"]) == 1, out)
    print("PASS a container that is not there stops it before anything changes")
    print("PROXMOX HUB-UP PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
