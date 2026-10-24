#!/usr/bin/env python3
"""Check what deploy/windows/install-devagotchi.ps1 left behind, then use it.

    python tests/windows_install_check.py --root DIR --claude-config FILE --workspace DIR

Asserts: endpoint.txt names a named pipe; broker.json is valid with the workspace and a
git grant; the Claude Desktop config kept every server it had and gained `devagotchi`
pointing at <root>\\current\\devagotchi.exe with mcp --socket <endpoint>; a backup was made.
Then runs tests/adapter_probe.py with exactly the registered command, so the entry Claude
Desktop would launch is the entry that is proven to work.
"""
import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--root", required=True, type=Path)
parser.add_argument("--claude-config", required=True, type=Path)
parser.add_argument("--workspace", required=True, type=Path)
parser.add_argument("--expect-kept", default="desktop-commander")
args = parser.parse_args()
failures = 0


def check(name, ok, detail=""):
    global failures
    print(f"{'ok  ' if ok else 'FAIL'} {name} {detail}".rstrip(), flush=True)
    failures += 0 if ok else 1


endpoint = (args.root / "endpoint.txt").read_text(encoding="utf-8").strip()
check("endpoint is a local named pipe", endpoint.startswith("\\\\.\\pipe\\devagotchi-broker-"), endpoint)

broker = json.loads((args.root / "config" / "broker.json").read_text(encoding="utf-8"))
check("broker.json workspace", Path(broker["workspace"]).resolve() == args.workspace.resolve(), broker["workspace"])
git = next((g for g in broker["allowed_executables"] if g.lower().endswith("\\git.exe")), None)
check("broker.json grants git", git is not None, json.dumps(broker["allowed_executables"]))

raw = args.claude_config.read_bytes()
check("claude config has no BOM", not raw.startswith(b"\xef\xbb\xbf"))
desktop = json.loads(raw.decode("utf-8"))
servers = desktop.get("mcpServers", {})
check("existing server kept", args.expect_kept in servers, ", ".join(servers))
entry = servers.get("devagotchi", {})
expected = str(args.root / "current" / "devagotchi.exe")
check("devagotchi command", entry.get("command", "").lower() == expected.lower(), entry.get("command", ""))
check("devagotchi args", entry.get("args") == ["mcp", "--socket", endpoint], json.dumps(entry.get("args")))
backups = list(args.claude_config.parent.glob(args.claude_config.name + ".bak-*"))
check("backup made", len(backups) >= 1, ", ".join(b.name for b in backups))

if git and entry.get("command"):
    probe = Path(__file__).resolve().parent / "adapter_probe.py"
    # Exactly the registered command: its executable and words, with the probe supplying
    # the trailing `--socket <endpoint>` the entry ends in.
    result = subprocess.run([sys.executable, str(probe), entry["args"][-1], "--adapter", entry["command"], *entry["args"][:-2],
                             "--git", git, "--cwd", str(args.workspace)], timeout=120)
    check("registered command works (adapter probe)", result.returncode == 0)

sys.exit(1 if failures else 0)
