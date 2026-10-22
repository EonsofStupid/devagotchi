#!/usr/bin/env python3
"""End to end on this machine with the built binaries: broker, stdio adapter, hub, link.

    python tests/e2e.py --bin-dir target/debug

Starts `bridge-broker` with a fresh private state directory and a workspace that grants
only git, drives it through `bridge-mcp` over stdio (tests/adapter_probe.py), then starts
`devagotchi-hub` in dev-token mode with two routes to that broker: `local` over IPC and
`remote` over the machine link. It drives the local route over Streamable HTTP MCP
(tests/hub_probe.py), then the link's whole life: not enrolled at first, a one-time
code that works once, `devagotchi-link run`, the same probe through the link, offline when
the link stops, back after it restarts, and a revoke that closes it and makes it exit 2.
Last, Shippin sign-in: a hub in `nomen` mode against a fake Nomen (tests/nomen_probe.py).
Linux uses a Unix socket, Windows a named pipe. Exit 0 only when every step passes.
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
broker_bin, adapter_bin, hub_bin, link_bin = (
    bins / f"{name}{EXE}" for name in ("bridge-broker", "bridge-mcp", "devagotchi-hub", "devagotchi-link")
)
for path in (broker_bin, adapter_bin, hub_bin, link_bin):
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


def run_cmd(argv, label, expect=0, env=None):
    """Run a one-shot command; stderr echoed, stdout returned. True when it exits `expect`."""
    print(f"--- {label}", flush=True)
    result = subprocess.run([str(a) for a in argv], capture_output=True, text=True, timeout=60,
                            env={**os.environ, **(env or {})}, stdin=subprocess.DEVNULL)
    for line in result.stderr.splitlines():
        print(f"  [{label}] {line}", flush=True)
    ok = result.returncode == expect
    print(f"--- {label}: exit {result.returncode}" + ("" if ok else f" (expected {expect})"), flush=True)
    return ok, result.stdout.strip()


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
        "brokers": [{"name": "local", "socket": endpoint}, {"name": "remote", "link": True}],
        "grants": [{"subject": "e2e-operator", "brokers": ["local", "remote"]}],
    }))
    hub_state = work / "hub-state"
    hub_url = f"http://127.0.0.1:{port}"
    start([str(hub_bin), "--state-dir", str(hub_state), "--config", str(hub_config)], "ready:", "hub")
    probe = [str(HERE / "hub_probe.py"), hub_url, "--token", token, "--git", git, "--cwd", str(workspace)]
    hub_ok = run_probe(probe + ["--broker", "local"], "hub probe")

    # The machine link: the same broker, reached the way gus or dabo would reach the hub.
    link_state = work / "link-state"
    steps = []
    steps.append(run_probe(probe + ["--broker", "remote", "--expect-error", "not_enrolled"], "linked broker before enrollment"))
    enrolled, code = run_cmd([hub_bin, "enroll", "--state-dir", hub_state, "--device", "remote"], "hub enroll")
    steps.append(enrolled and code.startswith("dvg-"))
    # The code travels in the environment, never on a command line other users can read.
    enroll = [link_bin, "enroll", "--hub", hub_url, "--device", "remote", "--broker", endpoint]
    code_env = {"DEVAGOTCHI_ENROLL_CODE": code}
    steps.append(run_cmd(enroll + ["--state-dir", link_state], "link enroll", env=code_env)[0])
    steps.append(run_cmd(enroll + ["--state-dir", work / "second-machine"], "link enroll with a spent code", expect=2, env=code_env)[0])
    link, _ = start([str(link_bin), "run", "--state-dir", str(link_state)], "ready:", "link")
    steps.append(run_probe(probe + ["--broker", "remote"], "hub probe through the link"))
    link.terminate()
    link.wait(10)
    steps.append(run_probe(probe + ["--broker", "remote", "--expect-error", "broker_offline"], "linked broker after the link stops"))
    link, _ = start([str(link_bin), "run", "--state-dir", str(link_state)], "ready:", "link")
    steps.append(run_probe(probe + ["--broker", "remote"], "hub probe after the link restarts"))
    steps.append(run_cmd([hub_bin, "revoke", "--state-dir", hub_state, "--device", "remote"], "hub revoke")[0])
    try:
        exit_code = link.wait(20)
    except subprocess.TimeoutExpired:
        exit_code = None
    print(f"--- revoked link exit {exit_code} (expected 2)", flush=True)
    steps.append(exit_code == 2)
    steps.append(run_probe(probe + ["--broker", "remote", "--expect-error", "device_revoked"], "revoked device refused"))
    link_ok = all(steps)
    print(f"--- link steps: {['ok' if s else 'FAIL' for s in steps]}", flush=True)

    # Shippin sign-in: a second hub that checks Nomen tokens, against a fake Nomen.
    nomen_ok = run_probe(
        [str(HERE / "nomen_probe.py"), "--hub-bin", str(hub_bin), "--endpoint", endpoint,
         "--git", git, "--cwd", str(workspace), "--work", str(work / "nomen")],
        "Nomen sign-in",
    )
    ok = adapter_ok and hub_ok and link_ok and nomen_ok
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
