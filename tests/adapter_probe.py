#!/usr/bin/env python3
"""Probe a running broker through the stdio MCP adapter.

    python3 tests/adapter_probe.py <broker endpoint> --adapter PATH [--git PATH] [--cwd DIR]

Speaks MCP over the adapter's stdio: initialize, list the tools, run `git --version`
under the broker in the given cwd, read its output, and confirm an ungranted
executable is refused. Exit 0 only when every step holds. The endpoint is the broker's
ready line: a Unix socket path, or a named pipe on Windows. A liveness receipt used by
tests/e2e.py and the deployment runbooks, not a conformance suite (that is
tests/mcp_smoke.py and tests/security_boundary.py).
"""
import argparse
import os
import json
import shlex
import subprocess
import time
import uuid

parser = argparse.ArgumentParser()
parser.add_argument("socket")
parser.add_argument("--adapter", required=True, help="the adapter command: a path, or words such as 'devagotchi mcp'")
parser.add_argument("--git", default="/usr/bin/git")
parser.add_argument("--cwd", default=None)
args = parser.parse_args()

proc = subprocess.Popen(
    [*shlex.split(args.adapter), "--socket", args.socket],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1,
)
request_id = 0


def rpc(method, params=None, notify=False):
    global request_id
    message = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        message["params"] = params
    if not notify:
        request_id += 1
        message["id"] = request_id
    proc.stdin.write(json.dumps(message) + "\n")
    proc.stdin.flush()
    if notify:
        return None
    while True:
        line = proc.stdout.readline()
        if not line:
            raise SystemExit("adapter closed: " + proc.stderr.read())
        reply = json.loads(line)
        if reply.get("id") == request_id:
            return reply


def call(tool, arguments):
    reply = rpc("tools/call", {"name": tool, "arguments": arguments})
    if "error" in reply:
        raise SystemExit(f"{tool} error: {reply['error']}")
    result = reply["result"]
    structured = result.get("structuredContent")
    if structured is None:
        structured = json.loads(result["content"][0]["text"])
    if result.get("isError"):
        raise SystemExit(f"{tool} isError: {structured}")
    return structured


init = rpc("initialize", {
    "protocolVersion": "2025-06-18", "capabilities": {},
    "clientInfo": {"name": "devspace-probe", "version": "0"},
})["result"]
print("server:", init["serverInfo"]["name"], init["serverInfo"].get("version"), "protocol", init["protocolVersion"])
rpc("notifications/initialized", notify=True)
tools = rpc("tools/list")["result"]["tools"]
print("tools:", ", ".join(sorted(t["name"] for t in tools)))

started = call("operation_start", {
    "idempotency_key": str(uuid.uuid4()), "executable": args.git, "args": ["--version"],
    "cwd": args.cwd, "deadline_ms": 30000,
})
operation = started["operation"]["operation_id"] if "operation" in started else started["operation_id"]
print("operation:", operation)
state = None
for _ in range(300):  # up to 30 s
    got = call("operation_get", {"operation_id": operation})
    state = got.get("operation", got)
    # Non-terminal states per OperationState::terminal (contracts): accepted, running.
    if state["state"] not in ("accepted", "running"):
        break
    time.sleep(0.1)
print("state:", state["state"], "exit:", state.get("exit_code", state.get("exit")))
read = call("operation_read", {"operation_id": operation, "cursor": 0, "max_bytes": 4096})
frames = read.get("frames", read.get("chunks", []))
text = "".join(f.get("text", f.get("data", "")) for f in frames) if frames else json.dumps(read)[:300]
print("output:", text.strip()[:200])
if state["state"] != "succeeded" or "git version" not in text:
    raise SystemExit("git --version did not succeed under the broker")

# An executable that exists on every machine of this OS and is never granted.
ungranted = r"C:\Windows\System32\whoami.exe" if os.name == "nt" else "/usr/bin/env"
denied = rpc("tools/call", {"name": "operation_start", "arguments": {
    "idempotency_key": str(uuid.uuid4()), "executable": ungranted, "args": [], "deadline_ms": 5000,
}})
result = denied.get("result", {})
refused = bool(result.get("isError")) or "error" in denied
detail = (result.get("content") or [{}])[0].get("text", str(denied.get("error", "")))[:140]
print(f"ungranted {ungranted} refused:", refused, detail)
if not refused or "executable_not_allowed" not in detail:
    raise SystemExit("an ungranted executable was not refused with executable_not_allowed")
proc.stdin.close()
proc.wait(timeout=5)
