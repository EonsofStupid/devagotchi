#!/usr/bin/env python3
"""A machine updates itself through its hub (docs/spec-machine-update.md, U5), on this machine.

    python tests/machine_update_probe.py --installer windows --a DIR_A --b DIR_B --work DIR
    sudo python3 tests/machine_update_probe.py --installer linux --a DIR_A --b DIR_B --work DIR
    python3 tests/machine_update_probe.py --installer none --a DIR_A --b DIR_B --work DIR

DIR_A and DIR_B each hold a `devagotchi` binary built with DEVAGOTCHI_BUILD=ci-a and ci-b. A
hub on this machine (dev tokens, `machines.auto_update` off) hands out builds the way a hub
does (`/dist`: archive, installer, VERSION, SHA256SUMS), and `machine_update` over MCP asks.

--installer windows|linux: the installer puts build A in place from local artifacts and joins
the hub (on Windows the broker and link then run as the HKCU Run entries start them at logon;
on Linux as systemd user services, the update applied by devagotchi-update.path). First the
hub serves a broken build whose broker exits at start: the machine must come back on ci-a and
`machines` must say `failed: …, back on ci-a`. Then it serves ci-b: the machine must come back
reporting ci-b, and asking again answers `already on ci-b`.

--installer none (tests/e2e.py, Linux): broker and link run bare; this probe plays the root
updater the .path unit starts (checks the request and the verified download, then restarts
the link on build B), and the machine goes from ci-a to ci-b.
"""
import argparse
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
WINDOWS = os.name == "nt"
EXE = ".exe" if WINDOWS else ""

parser = argparse.ArgumentParser()
parser.add_argument("--installer", choices=["windows", "linux", "none"], required=True)
parser.add_argument("--a", required=True, type=Path, help="directory holding build ci-a")
parser.add_argument("--b", required=True, type=Path, help="directory holding build ci-b")
parser.add_argument("--work", required=True, type=Path)
args = parser.parse_args()
work = args.work.resolve()
work.mkdir(parents=True, exist_ok=True)
binary_a = args.a.resolve() / f"devagotchi{EXE}"
binary_b = args.b.resolve() / f"devagotchi{EXE}"
processes = []


def check(label, ok, detail=""):
    print(f"{'ok  ' if ok else 'FAIL'} {label}{(': ' + detail) if detail and not ok else ''}", flush=True)
    if not ok:
        raise SystemExit(1)


def installer_path():
    return ROOT / ("deploy/windows/install-devagotchi.ps1" if WINDOWS else "deploy/linux/install-devagotchi.sh")


def target():
    if WINDOWS:
        out = subprocess.run(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                              str(installer_path()), "-PrintTarget"], capture_output=True, text=True)
    else:
        out = subprocess.run(["bash", str(installer_path()), "--print-target"], capture_output=True, text=True)
    return out.stdout.strip()


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def lay_down(name, binary):
    """A hub's downloads directory handing out `binary` as version `name`."""
    dist = work / f"dist-{name}"
    shutil.rmtree(dist, ignore_errors=True)
    dist.mkdir()
    if WINDOWS:
        archive = f"devagotchi-{target()}.zip"
        with zipfile.ZipFile(dist / archive, "w") as z:
            z.write(binary, "devagotchi.exe")
    else:
        archive = f"devagotchi-{target()}.tar.xz"
        with tarfile.open(dist / archive, "w:xz", preset=0) as t:  # fast: debug builds are large
            info = t.gettarinfo(str(binary), f"devagotchi-{target()}/devagotchi")
            info.mode = 0o755
            with open(binary, "rb") as f:
                t.addfile(info, f)
    shutil.copy(installer_path(), dist / installer_path().name)
    (dist / f"{archive}.sha256").write_text(f"{sha256(dist / archive)}  {archive}\n")
    (dist / "VERSION").write_text(f"{name}\n")
    sums = "".join(f"{sha256(dist / f)}  {f}\n" for f in (archive, installer_path().name))
    (dist / "SHA256SUMS").write_text(sums)
    os.chmod(dist, 0o755)
    for f in dist.iterdir():
        os.chmod(f, 0o644)
    return dist


def broken_binary():
    """A build whose broker exits at start."""
    folder = work / "broken"
    folder.mkdir(exist_ok=True)
    path = folder / f"devagotchi{EXE}"
    if WINDOWS:
        # whoami refuses `broker run …` and exits 1 at once, printing no ready line.
        shutil.copy(Path(os.environ["SystemRoot"]) / "System32" / "whoami.exe", path)
    else:
        path.write_text("#!/bin/sh\necho 'a broken build' >&2\nexit 1\n")
        path.chmod(0o755)
    return path


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


port = free_port()
hub_url = f"http://127.0.0.1:{port}"
hub_state = work / "hub-state"
tokens = work / "tokens.json"
token = "ci-update-" + os.urandom(12).hex()
tokens.write_text(json.dumps({token: "ci"}))
if not WINDOWS:
    tokens.chmod(0o600)
hub = {"process": None}


def start_hub(dist):
    """(Re)start the hub handing out `dist`; its state (the enrollment) is kept."""
    if hub["process"]:
        hub["process"].terminate()
        hub["process"].wait(10)
    config = work / "hub.json"
    config.write_text(json.dumps({
        "listen": f"127.0.0.1:{port}", "public_url": hub_url,
        "auth": {"mode": "dev", "tokens_file": str(tokens)},
        "brokers": [{"name": "m1", "link": True}],
        "grants": [{"subject": "ci", "brokers": ["m1"]}],
        "downloads": str(dist),
        "machines": {"auto_update": False},
    }))
    log = open(work / "hub.log", "a")
    process = subprocess.Popen([str(binary_a), "hub", "serve", "--state-dir", str(hub_state), "--config", str(config)],
                               stdin=subprocess.DEVNULL, stdout=log, stderr=log)
    processes.append(process)
    hub["process"] = process
    for _ in range(300):
        try:
            urllib.request.urlopen(hub_url + "/healthz", timeout=2)
            return
        except (urllib.error.URLError, OSError):
            time.sleep(0.1)
    raise SystemExit(f"the hub did not start; see {work / 'hub.log'}")


def call(tool, arguments, timeout=120):
    """One MCP tool call: initialize, then tools/call, over Streamable HTTP."""
    session = {}

    def rpc(rid, method, params):
        body = json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}).encode()
        req = urllib.request.Request(hub_url + "/mcp", data=body, method="POST")
        req.add_header("Accept", "application/json, text/event-stream")
        req.add_header("Content-Type", "application/json")
        req.add_header("Authorization", "Bearer " + token)
        req.add_header("MCP-Protocol-Version", "2025-06-18")
        if "id" in session:
            req.add_header("Mcp-Session-Id", session["id"])
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            session.setdefault("id", resp.headers.get("Mcp-Session-Id"))
            text = resp.read().decode()
            if resp.headers.get("Content-Type", "").startswith("text/event-stream"):
                data = [line[5:].strip() for line in text.splitlines() if line.startswith("data:")]
                text = data[-1] if data else "{}"
        return json.loads(text)

    rpc(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                          "clientInfo": {"name": "machine-update-probe", "version": "0"}})
    result = rpc(2, "tools/call", {"name": tool, "arguments": arguments})
    if "error" in result:
        raise SystemExit(f"{tool}: {result['error']}")
    return result["result"].get("structuredContent") or {}


def machine():
    machines = call("machines", {}).get("machines", [])
    return next((m for m in machines if m["name"] == "m1"), {})


def wait_for(predicate, label, seconds=300):
    deadline = time.time() + seconds
    seen = {}
    while time.time() < deadline:
        try:
            seen = machine()
        except (urllib.error.URLError, OSError):
            seen = {}
        if predicate(seen):
            return seen
        time.sleep(2)
    check(label, False, f"machines said {seen}")


def online_on(build):
    return lambda m: m.get("status") == "online" and m.get("build") == build and m.get("update") != "updating"


def enroll_code():
    out = subprocess.run([str(binary_a), "hub", "enroll", "--state-dir", str(hub_state), "--device", "m1"],
                         capture_output=True, text=True)
    code = out.stdout.strip()
    check("the hub issued an enrollment code", code.startswith("dvg-"), out.stderr)
    return code


# ---------------------------------------------------------------- the machine on build A
def install_windows(dist_a):
    root, workspace = work / "machine", work / "ws"
    with open(work / "install-a.log", "w") as log:
        result = subprocess.run(
            ["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(installer_path()),
             "-Version", "ci-a", "-ArtifactDir", str(dist_a), "-Root", str(root), "-Workspace", str(workspace),
             "-Autostart", "-Hub", hub_url, "-Device", "m1"],
            env={**os.environ, "DEVAGOTCHI_ENROLL_CODE": enroll_code()},
            stdin=subprocess.DEVNULL, stdout=log, stderr=log)  # a file: the broker inherits it
    check("the installer put ci-a in place and joined the hub", result.returncode == 0,
          (work / "install-a.log").read_text(errors="replace")[-2000:])
    # Then as logon starts them: stop the ones the installer started, run the Run entries.
    stop = (f"Get-CimInstance Win32_Process -Filter \"Name = 'devagotchi.exe'\" | Where-Object {{ $_.ExecutablePath -and "
            f"$_.ExecutablePath.StartsWith('{root}', [System.StringComparison]::OrdinalIgnoreCase) }} | "
            "ForEach-Object { Stop-Process -Id $_.ProcessId -Force }")
    subprocess.run(["powershell", "-NoProfile", "-Command", stop], check=True)
    import winreg
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run") as key:
        for name in ("Devagotchi Broker", "Devagotchi Link"):
            line = winreg.QueryValueEx(key, name)[0]
            subprocess.Popen(line, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             creationflags=0x00000008 | 0x00000200)  # detached, as logon starts it
    return lambda: subprocess.run(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                                   str(installer_path()), "-Root", str(root), "-Uninstall"])


LINUX_ACCOUNT = ["--user", "dvgupdate", "--home", "/var/lib/dvgupdate", "--prefix", "/opt/dvgupdate"]


def install_linux(dist_a):
    with open(work / "install-a.log", "w") as log:
        result = subprocess.run(["bash", str(installer_path()), "--version", "ci-a", "--artifact-dir", str(dist_a),
                                 "--hub", hub_url, "--device", "m1", "--enroll", *LINUX_ACCOUNT],
                                input=enroll_code() + "\n", text=True, stdout=log, stderr=log)
    check("the installer put ci-a in place and joined the hub", result.returncode == 0,
          (work / "install-a.log").read_text(errors="replace")[-2000:])
    unit = Path("/etc/systemd/system/devagotchi-update.path").read_text()
    check("devagotchi-update.path watches the link's request", "update-request.json" in unit, unit)
    return lambda: subprocess.run(["bash", str(installer_path()), "--uninstall", *LINUX_ACCOUNT])


def run_bare(dist_a):
    """Broker and link from build A, unprivileged; returns the link's state directory."""
    state, config, ws = work / "broker-state", work / "broker.json", work / "ws"
    ws.mkdir(exist_ok=True)
    config.write_text(json.dumps({"workspace": str(ws), "allowed_executables": [], "output_limit_bytes": 65536,
                                  "max_operations": 8, "max_concurrent": 2}))
    log = open(work / "broker.log", "w")
    broker = subprocess.Popen([str(binary_a), "broker", "run", "--state-dir", str(state), "--config", str(config)],
                              stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.PIPE, text=True)
    processes.append(broker)
    line = broker.stderr.readline()
    check("the broker on ci-a is ready", "ready:" in line, line)
    endpoint = line.split("ready:", 1)[1].strip()
    link_state = work / "link-state"
    subprocess.run([str(binary_a), "link", "enroll", "--state-dir", str(link_state), "--hub", hub_url,
                    "--device", "m1", "--broker", endpoint],
                   env={**os.environ, "DEVAGOTCHI_ENROLL_CODE": enroll_code()}, check=True)
    return link_state


def start_link(binary, link_state):
    log = open(work / "link.log", "a")
    process = subprocess.Popen([str(binary), "link", "run", "--state-dir", str(link_state)],
                               stdin=subprocess.DEVNULL, stdout=log, stderr=log)
    processes.append(process)
    return process


uninstall = None
ok = False
try:
    dist_b = lay_down("ci-b", binary_b)
    dist_a = lay_down("ci-a", binary_a)
    start_hub(dist_b if args.installer == "none" else lay_down("ci-broken", broken_binary()))
    if args.installer == "none":
        link_state = run_bare(dist_a)
        link = start_link(binary_a, link_state)
        wait_for(online_on("ci-a"), "the machine is online on ci-a", 60)
        print("ok   the machine is online on ci-a", flush=True)
        asked = call("machine_update", {"machine": "m1", "wait_seconds": 1})
        check("machine_update asked the machine", asked.get("state") in ("requested", "downloading", "installing", "waiting_for_jobs"), str(asked))
        # The .path unit's part: the request names the verified download; the link restarts on it.
        request = link_state / "update-request.json"
        for _ in range(300):
            if request.exists():
                break
            time.sleep(0.1)
        check("the link asked for the update with update-request.json", request.exists())
        asked = json.loads(request.read_text())
        request.unlink()
        downloaded = Path(asked["dir"])
        archive = next(downloaded.glob("devagotchi-*.tar.xz"))
        check("the request names ci-b and its checked download",
              asked["version"] == "ci-b" and sha256(archive) == sha256(dist_b / archive.name)
              and (downloaded / f"{archive.name}.sha256").read_text().split()[0] == sha256(archive), str(asked))
        link.terminate()
        link.wait(10)
        link = start_link(binary_b, link_state)
        wait_for(online_on("ci-b"), "the machine came back on ci-b")
        print("ok   machine_update took the machine from ci-a to ci-b", flush=True)
    else:
        uninstall = (install_windows if WINDOWS else install_linux)(dist_a)
        wait_for(online_on("ci-a"), "the machine is online on ci-a", 120)
        print("ok   the machine runs ci-a as it starts at logon or boot", flush=True)
        # A broken build: the machine comes back on the build it had, and says why.
        asked = call("machine_update", {"machine": "m1", "wait_seconds": 50})
        print(f"     machine_update to ci-broken: {asked}", flush=True)
        failed = wait_for(lambda m: online_on("ci-a")(m) and str(m.get("update", "")).startswith("failed:"),
                          "the machine came back on ci-a and machines says the update failed")
        check("machines says why and which build it is back on", failed["update"].endswith(", back on ci-a"),
              failed["update"])
        print(f"ok   a broken build rolled back: {failed['update']}", flush=True)
        # A good build: the machine comes back on it.
        start_hub(dist_b)
        wait_for(online_on("ci-a"), "the machine reconnected to the restarted hub", 180)
        asked = call("machine_update", {"machine": "m1", "wait_seconds": 50})
        print(f"     machine_update to ci-b: {asked}", flush=True)
        wait_for(lambda m: online_on("ci-b")(m) and "update" not in m, "the machine came back reporting ci-b")
        print("ok   the machine updated itself to ci-b", flush=True)
    again = call("machine_update", {"machine": "m1"})
    check("asking again answers already on ci-b", again.get("detail") == "already on ci-b", str(again))
    ok = True
finally:
    if uninstall:
        uninstall()
    for process in reversed(processes):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(10)
            except subprocess.TimeoutExpired:
                process.kill()
    if not ok:
        for log in sorted(work.glob("*.log")) + sorted(work.glob("machine/logs/*.log")) + sorted(Path("/opt/dvgupdate/update").glob("*.log")):
            print(f"--- {log}", flush=True)
            print(log.read_text(errors="replace")[-3000:], flush=True)

print("MACHINE UPDATE PASSED" if ok else "MACHINE UPDATE FAILED", flush=True)
sys.exit(0 if ok else 1)
