#!/usr/bin/env python3
"""Probe a running broker through the stdio MCP adapter.

    python3 tests/adapter_probe.py <broker endpoint> --adapter EXE [WORD...] [--git PATH] [--cwd DIR]

Speaks MCP over the adapter's stdio: initialize, list the tools, run `git --version`
under the broker in the given cwd, read its output, and confirm an ungranted
executable, and `run`'s command line, are refused (with what to do instead). With
--account the broker runs as its account: then a command line runs through `run`, and a
file in the cwd is written, edited, read and listed, each in one call, and
`operation_list` shows the run. Exit 0 only when every step holds. The endpoint is the broker's
ready line: a Unix socket path, or a named pipe on Windows. The adapter command is given
as its words (`--adapter target/debug/devagotchi mcp`), never as one string to split:
a Windows path is full of backslashes. The probe adds `--socket <endpoint>` itself.
A liveness receipt used by tests/e2e.py and the deployment runbooks, not a conformance
suite (that is tests/mcp_smoke.py and tests/security_boundary.py).
"""
import argparse
import os
import json
import subprocess
import time
import uuid

parser = argparse.ArgumentParser()
parser.add_argument("socket")
parser.add_argument("--adapter", required=True, nargs="+", metavar="WORD",
                    help="the adapter command, word by word: the executable, then its words (e.g. devagotchi mcp)")
parser.add_argument("--git", default="/usr/bin/git")
parser.add_argument("--cwd", default=None)
parser.add_argument("--account", action="store_true",
                    help="the broker runs as its account (exec_policy account): probe the one-call tools")
args = parser.parse_args()

proc = subprocess.Popen(
    [*args.adapter, "--socket", args.socket],
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
missing = {"run", "read_file", "list_directory", "write_file", "edit_file", "operation_list",
           "operation_start", "operation_wait"} - {t["name"] for t in tools}
if missing:
    raise SystemExit(f"tools missing: {sorted(missing)}")

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

if args.account:
    # Desktop Commander's everyday calls, one call each. No `&&`: Windows PowerShell 5.1
    # has none, and the line must read the same on every system.
    ran = call("run", {"command": "echo devagotchi-local-run"})
    print("run:", ran["state"], ran.get("exit_code"), ran["output"].strip()[:80])
    if ran["state"] != "succeeded" or "devagotchi-local-run" not in ran["output"]:
        raise SystemExit("run did not return the command's output")
    path = os.path.join(args.cwd, "local-tools.txt")
    written = call("write_file", {"path": path, "text": "one line\n"})
    edited = call("edit_file", {"path": path, "old_text": "one", "new_text": "two"})
    content = call("read_file", {"path": path})
    listing = call("list_directory", {"path": args.cwd})
    print("file:", written["created"], edited["replacements"], repr(content.get("text")))
    if not written["created"] or edited["replacements"] != 1 or content.get("text") != "two line\n":
        raise SystemExit("write_file, edit_file and read_file did not round-trip")
    if "local-tools.txt" not in [entry["path"] for entry in listing["entries"]]:
        raise SystemExit("list_directory did not list the written file")
    # By lines, as Desktop Commander reads: a line from a number, then the last line.
    numbered = os.path.join(args.cwd, "lines.txt")
    call("write_file", {"path": numbered, "text": "alpha\nbeta\ngamma\n"})
    middle = call("read_file", {"path": numbered, "start_line": 2, "line_count": 1})
    last = call("read_file", {"path": numbered, "start_line": -1})
    print("lines:", repr(middle.get("text")), middle.get("next_line"), repr(last.get("text")), last.get("start_line"))
    if middle.get("text") != "beta\n" or middle.get("next_line") != 3 or middle.get("total_lines") != 3:
        raise SystemExit("read_file by lines did not read line 2")
    if last.get("text") != "gamma\n" or last.get("start_line") != 3:
        raise SystemExit("read_file did not read the last line")
    # The rest of Desktop Commander's file tools: a directory, a move, info, several files.
    made = call("create_directory", {"path": os.path.join(args.cwd, "made", "deeper")})
    moved = call("move_file", {"source": path, "destination": os.path.join(args.cwd, "made", "moved.txt")})
    info = call("get_file_info", {"path": moved["path"]})
    both = call("read_multiple_files", {"paths": [moved["path"], os.path.join(args.cwd, "missing.txt")]})["files"]
    print("manage:", made.get("made"), moved.get("replaced"), info.get("lines"),
          [bool(f.get("content")) for f in both], [f.get("error", {}).get("code") for f in both])
    if made.get("made") is not True or made.get("kind") != "dir":
        raise SystemExit("create_directory did not make the directory")
    if moved.get("replaced") is not False or os.path.exists(path):
        raise SystemExit("move_file did not move the file")
    if info.get("kind") != "file" or info.get("lines") != 1:
        raise SystemExit("get_file_info did not describe the moved file")
    if (both[0].get("content") or {}).get("text") != "two line\n" or both[1].get("error", {}).get("code") != "path_not_found":
        raise SystemExit("read_multiple_files did not read one file and report the other")
    operations = call("operation_list", {})["operations"]
    if ran["operation_id"] not in [op["operation_id"] for op in operations]:
        raise SystemExit("operation_list did not show the run")
    proc.stdin.close()
    proc.wait(timeout=5)
    raise SystemExit(0)

# A command line under the granted policy: refused, with what works instead.
denied = rpc("tools/call", {"name": "run", "arguments": {"command": "echo hi"}})
result = denied.get("result", {})
detail = (result.get("content") or [{}])[0].get("text", str(denied.get("error", "")))[:400]
print("run refused under granted:", bool(result.get("isError")), detail[:140])
if not result.get("isError") or "command_not_allowed" not in detail or "operation_start" not in detail:
    raise SystemExit("run was not refused with command_not_allowed and what to do instead")

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
