#!/usr/bin/env python3
"""End to end on this machine with the built binaries: broker, stdio adapter, hub.

    python tests/e2e.py --bin-dir target/debug

Starts `bridge-broker` with a fresh private state directory and a workspace that grants
only git, drives it through `bridge-mcp` over stdio (tests/adapter_probe.py),
then starts `devagotchi-hub` in dev-token mode in front of the same broker and drives it
over Streamable HTTP MCP (tests/hub_probe.py). Linux uses a Unix socket, Windows a named
pipe; the probes and the binaries are the same. Exit 0 only when both probes pass.
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
EXE = ".exe" if os.name == "nt" else ""

parser = argparse.ArgumentParser()
parser.add_argument("--bin-dir", required=True, type=Path)
parser.add_argument("--keep", action="store_true", help="keep the work directory")
args = parser.parse_args()
bins = args.bin_dir.resolve()
broker_bin, adapter_bin, hub_bin = (bins / f"{name}{EXE}" for name in ("bridge-broker", "bridge-mcp", "devagotchi-hub"))
for path in (broker_bin, adapter_bin, hub_bin):
    if not path.is_file():
        sys.exit(f"missing binary: {path} (cargo build --locked --bins)")

git = shutil.which("git")
if not git:
    sys.exit("git not found on PATH")
git = os.path.realpath(git)

work = Path(tempfile.mkdtemp(prefix="dvg-e2e-"))
workspace = work / "workspace"
workspace.mkdir()
state = work / "broker-state"          # created by the broker itself, privately
config = work / "broker.json"
config.write_text(json.dumps({
    "workspace": str(workspace),
    "allowed_executables": [git],
    "output_limit_bytes": 262144,
    "max_operations": 64,
    "max_concurrent": 4,
}))
processes = []


def start(argv, ready_marker, name):
    """Start a process, return (process, ready line). stderr is echoed with a prefix."""
    process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    processes.append(process)
    ready = {}
    event = threading.Event()

    def pump():
        for line in process.stderr:
            line = line.rstrip()
            print(f"  [{name}] {line}", flush=True)
            if ready_marker in line and "line" not in ready:
                ready["line"] = line
                event.set()
        event.set()

    threading.Thread(target=pump, daemon=True).start()
    if not event.wait(30) or "line" not in ready:
        sys.exit(f"{name} did not become ready (exit {process.poll()})")
    return process, ready["line"]


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def run_probe(argv, label):
    print(f"--- {label}", flush=True)
    result = subprocess.run([sys.executable, *argv], timeout=120)
    print(f"--- {label}: exit {result.returncode}", flush=True)
    return result.returncode == 0


ok = False
try:
    print(f"work dir {work}; git {git}", flush=True)
    _, ready = start([str(broker_bin), "--state-dir", str(state), "--config", str(config)], "ready:", "broker")
    endpoint = ready.split("ready:", 1)[1].strip()
    print(f"broker endpoint {endpoint}", flush=True)

    adapter_ok = run_probe(
        [str(HERE / "adapter_probe.py"), endpoint, "--adapter", str(adapter_bin), "--git", git, "--cwd", str(workspace)],
        "stdio adapter probe",
    )

    token = "e2e-dev-token-" + os.urandom(12).hex()
    tokens = work / "tokens.json"
    tokens.write_text(json.dumps({token: "e2e-operator"}))
    if os.name != "nt":
        tokens.chmod(0o600)
    port = free_port()
    hub_config = work / "hub.json"
    hub_config.write_text(json.dumps({
        "listen": f"127.0.0.1:{port}",
        "public_url": f"http://127.0.0.1:{port}",
        "auth": {"mode": "dev", "tokens_file": str(tokens)},
        "brokers": [{"name": "local", "socket": endpoint}],
        "grants": [{"subject": "e2e-operator", "brokers": ["local"]}],
    }))
    start([str(hub_bin), "--state-dir", str(work / "hub-state"), "--config", str(hub_config)], "ready:", "hub")
    hub_ok = run_probe(
        [str(HERE / "hub_probe.py"), f"http://127.0.0.1:{port}", "--token", token, "--broker", "local", "--git", git, "--cwd", str(workspace)],
        "hub probe",
    )
    ok = adapter_ok and hub_ok
finally:
    for process in reversed(processes):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(10)
            except subprocess.TimeoutExpired:
                process.kill()
    if not args.keep:
        time.sleep(0.5)
        shutil.rmtree(work, ignore_errors=True)

print("E2E PASSED" if ok else "E2E FAILED", flush=True)
sys.exit(0 if ok else 1)
