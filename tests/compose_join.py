#!/usr/bin/env python3
"""A hub from the image hands out builds with nothing configured, and one line joins a machine.

Runs against a deploy/compose stack already up from the hub image this commit built, hosted
(`DEVAGOTCHI_HUB_TENANCY=multi`) with no `downloads` anywhere in its config or environment
(image.yml, `hub image / build`). Then, on this runner, as a person would:

1. `/healthz` reports the builds the image carries (its VERSION, six files), and `/dist`
   serves every file `SHA256SUMS` lists with that checksum: the Linux archive holds the
   `devagotchi` binary and the Windows zip `devagotchi.exe`, for x86_64 and ARM64.
2. A person's AI client connects (dynamic registration, Connect, PKCE) and calls
   `machine_add`. The Windows line's script names the checksum of both Windows builds and
   of the PowerShell installer, as served.
3. The Linux line runs exactly as `machine_add` gave it (`curl -fsSL … | sudo sh`): it
   installs the hub's own build, enrolls the machine and starts it.
4. The machine is online, the AI runs a command on it with `run`, and the machine runs the
   hub's build (`/opt/devagotchi/current` -> `versions/<VERSION>`).

Needs root through passwordless sudo and systemd (a GitHub Ubuntu runner).

    python3 tests/compose_join.py --hub http://localhost:8787 --version main-abc1234
"""

import argparse
import hashlib
import io
import json
import lzma
import os
import re
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from operator_browser import Person, call  # noqa: E402

ARCHIVES = {
    "devagotchi-x86_64-unknown-linux-gnu.tar.xz": "devagotchi",
    "devagotchi-aarch64-unknown-linux-gnu.tar.xz": "devagotchi",
    "devagotchi-x86_64-pc-windows-msvc.zip": "devagotchi.exe",
    "devagotchi-aarch64-pc-windows-msvc.zip": "devagotchi.exe",
}


def ok(text: str) -> None:
    print(f"PASS {text}", flush=True)


def get(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=60) as answer:
        assert answer.status == 200, (url, answer.status)
        return answer.read()


def reachable(hub: str, seconds: int = 60) -> bytes:
    """`/healthz` from this host once the published port answers. `docker compose up --wait`
    returns when the container's own health check passes, which is inside the container; the
    host's forwarded port can still reset a connection for a moment after that (seen on the
    ARM runner: ConnectionResetError on the first request). Only connection-level errors are
    retried, each one printed, for at most `seconds`; any HTTP answer is final."""
    deadline = time.monotonic() + seconds
    while True:
        try:
            return get(f"{hub}/healthz")
        except (ConnectionError, urllib.error.URLError) as why:
            if isinstance(why, urllib.error.HTTPError) or time.monotonic() > deadline:
                raise
            print(f"waiting for {hub}/healthz from this host: {why}", flush=True)
            time.sleep(1)


def names_in(name: str, data: bytes) -> set:
    """The base names of the files in one archive."""
    if name.endswith(".zip"):
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            return {Path(entry).name for entry in archive.namelist()}
    with tarfile.open(fileobj=io.BytesIO(lzma.decompress(data))) as archive:
        return {Path(member.name).name for member in archive.getmembers() if member.isfile()}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--hub", required=True, help="the hub's public URL")
    parser.add_argument("--version", required=True, help="the VERSION the image carries")
    args = parser.parse_args()
    hub = args.hub.rstrip("/")

    # 1. The builds the image carries, with nothing configured.
    health = json.loads(reachable(hub))
    assert health["downloads"] == {"version": args.version, "files": 6}, health["downloads"]
    sums = {}
    for line in get(f"{hub}/dist/SHA256SUMS").decode().splitlines():
        digest, name = line.split()
        sums[name] = digest
    assert set(sums) == set(ARCHIVES) | {"install-devagotchi.sh", "install-devagotchi.ps1"}, sums
    for name, digest in sums.items():
        data = get(f"{hub}/dist/{name}")
        assert hashlib.sha256(data).hexdigest() == digest, name
        if name in ARCHIVES:
            assert ARCHIVES[name] in names_in(name, data), (name, names_in(name, data))
    ok(f"the image's hub hands out {args.version} with nothing configured: six files, each as listed")

    # 2. A person's AI connects and adds a machine; the Windows line is ready to run.
    person = Person(hub)
    added = person.tool("machine_add", {"name": "joined"})
    assert not added.get("isError"), added
    machine = added["structuredContent"]
    assert machine["linux"] == f"curl -fsSL {hub}/join/{machine['code']} | sudo sh", machine
    assert machine["windows"] == f"irm {hub}/join/{machine['code']}.ps1 | iex", machine
    status, _, script = call(f"{hub}/join/{machine['code']}.ps1")
    assert status == 200, (status, script)
    for name in ["devagotchi-x86_64-pc-windows-msvc.zip", "devagotchi-aarch64-pc-windows-msvc.zip",
                 "install-devagotchi.ps1"]:
        assert sums[name] in script, (name, script)
    assert "$version = '" + args.version + "'" in script, script
    assert not re.search(r"\$sum = ''", script), script
    ok("an AI's machine_add gives both lines; the Windows script names both Windows builds as served")

    # 3. The Linux line, exactly as given (fetching the scripts above spent nothing).
    joined = subprocess.run(["bash", "-c", machine["linux"]], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=600)
    print(joined.stderr[-4000:], file=sys.stderr)
    assert joined.returncode == 0, joined.stdout[-2000:]
    assert f"installing devagotchi {args.version} and joining {hub}" in joined.stderr, joined.stderr
    ok("the Linux line installed the hub's build and joined")

    # 4. Online, and the AI runs a command on it.
    ran = None
    for _ in range(60):
        ran = person.tool("run", {"machine": "joined", "command": "echo joined-from-the-image"})
        if not ran.get("isError"):
            break
        time.sleep(1)
    assert not ran.get("isError"), ran
    assert "joined-from-the-image" in ran["structuredContent"]["output"], ran
    current = subprocess.run(["sudo", "readlink", "/opt/devagotchi/current"],
                             capture_output=True, text=True, check=True).stdout.strip()
    assert current == f"versions/{args.version}", current
    listed = person.tool("machines")["structuredContent"]["machines"]
    assert any(m.get("status") == "online" for m in listed), listed
    ok(f"the machine is online, runs {args.version}, and the AI ran a command on it")
    print("COMPOSE JOIN PASSED")
    return 0


if __name__ == "__main__":
    os.umask(0o022)
    sys.exit(main())
