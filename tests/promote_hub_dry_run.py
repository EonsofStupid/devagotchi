#!/usr/bin/env python3
"""Dry-run of deploy/linux/promote-hub.sh live mode.

Checks the plan that a hand deploy to one container needs: the installer is
pushed with the artifact, downloads are staged before that install, rollback
starts the container, and ssh gets the key and SSH_OPTS.
"""
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "deploy" / "linux" / "promote-hub.sh"


def run(extra, env):
    result = subprocess.run(
        [str(SCRIPT), "--confirm", "--dry-run", *extra],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        sys.stderr.write(result.stdout)
        sys.stderr.write(result.stderr)
        raise SystemExit(f"dry-run exited {result.returncode}")
    return result.stdout + result.stderr


def main():
    sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    with tempfile.TemporaryDirectory() as tmp:
        artifact = Path(tmp) / "artifact"
        dist = Path(tmp) / "dist"
        artifact.mkdir()
        dist.mkdir()
        (artifact / "devagotchi-x86_64-unknown-linux-gnu.tar.xz").write_bytes(b"xz")
        (artifact / "devagotchi-x86_64-unknown-linux-gnu.tar.xz.sha256").write_text("abc  archive\n")
        key = Path(tmp) / "id_ed25519"
        key.write_text("not a real key\n")
        env = os.environ.copy()
        env.update(
            {
                "DEVAGOTCHI_PROMOTE_TARGET": "live",
                "DEVAGOTCHI_SHA": sha,
                "DEVAGOTCHI_VERSION": "test-build",
                "DEVAGOTCHI_PREVIOUS_VERSION": "previous-build",
                "DEVAGOTCHI_PROD_HOST": "root@203.0.113.10",
                "DEVAGOTCHI_PROD_LXC": "110",
                "DEVAGOTCHI_PROD_URL": "https://app.devagotchi.com",
                "DEVAGOTCHI_DIST_DIR": str(dist),
                "DEVAGOTCHI_DOWNLOADS_DIR": "/var/lib/devagotchi/downloads",
                "SSH_OPTS": "-o BatchMode=yes",
            }
        )
        out = run(
            ["--artifact", str(artifact), "--ssh-key", str(key), "--ssh-opts", "-p 2222"],
            env,
        )
        def at(needle):
            spot = out.find(needle)
            if spot < 0:
                raise SystemExit(f"missing {needle!r} in:\n{out}")
            return spot

        staged = at("before the hub starts")
        pushed = at("/tmp/devagotchi-promote/install-devagotchi.sh")
        if not staged < pushed:
            raise SystemExit(f"downloads were not staged before the installer:\n{out}")
        if "--artifact-dir /tmp/devagotchi-promote" not in out:
            raise SystemExit(f"installer was not pointed at the pushed artifact:\n{out}")
        if "install-devagotchi.sh --role hub --version test-build" not in out.replace(
            "/tmp/devagotchi-promote/install-devagotchi.sh", "install-devagotchi.sh"
        ):
            raise SystemExit(f"live install still expects a GitHub release on PATH:\n{out}")
        rollback = at("pct rollback 110 devagotchi-pre-test-build --start")
        if "--start" not in out[rollback:rollback + 80]:
            raise SystemExit(f"rollback does not start the container:\n{out}")
        ssh = at("dry-run: ssh -i ")
        ssh_line = out[ssh:].splitlines()[0]
        if str(key) not in ssh_line or "-o BatchMode=yes" not in ssh_line or "-p 2222" not in ssh_line:
            raise SystemExit(f"ssh command ignores the key or SSH_OPTS:\n{ssh_line}")

        url_out = run(
            ["--artifact", "https://example.invalid/devagotchi-ci.tar.gz"],
            env,
        )
        if "curl -fsSL -o" not in url_out or "https://example.invalid/devagotchi-ci.tar.gz" not in url_out:
            raise SystemExit(f"a URL artifact was not fetched:\n{url_out}")
    print("promote-hub dry-run ok")


if __name__ == "__main__":
    main()
