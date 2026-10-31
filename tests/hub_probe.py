#!/usr/bin/env python3
"""Probe a running hub (`devagotchi hub serve`) over Streamable HTTP MCP.

    python3 tests/hub_probe.py http://127.0.0.1:8787 --token <bearer> --broker <name> [--git /usr/bin/git] [--cwd DIR]
    python3 tests/hub_probe.py http://127.0.0.1:8787 --token <bearer> --broker <name> --expect-error broker_offline

Checks, in order: an unauthenticated call is refused with a 401 that names the
protected-resource metadata; /healthz answers; initialize and tools/list; whoami names
the broker and `machines` lists it online; operation_start of `git --version` on that
broker succeeds and its output reads back; a broker the subject holds no grant for is
refused with `not_granted`.
With --expect-error CODE it instead checks that one routed call to the broker fails
with exactly CODE (a linked machine that is offline or revoked). Exit 0 only when every
step holds.
"""
import argparse
import json
import sys
import time
import urllib.error
import urllib.request
import uuid

parser = argparse.ArgumentParser()
parser.add_argument("base")
parser.add_argument("--token", required=True)
parser.add_argument("--broker", required=True)
parser.add_argument("--ungranted", default="nobody-has-this")
parser.add_argument("--git", default="/usr/bin/git")
parser.add_argument("--cwd", default=None)
parser.add_argument("--expect-error", default=None)
# A broker under the `account` policy to exercise `run` on (one call: start, wait, output).
parser.add_argument("--run-broker", default=None)
# An absolute directory on that broker's machine (this host in the harness) for the file tools.
parser.add_argument("--run-dir", default=None)
args = parser.parse_args()
base = args.base.rstrip("/")
session = {"id": None, "rid": 0}


def http(method, path, body=None, headers=None, token=args.token):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(base + path, data=data, method=method)
    req.add_header("Accept", "application/json, text/event-stream")
    if data is not None:
        req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    req.add_header("MCP-Protocol-Version", "2025-06-18")
    if session["id"]:
        req.add_header("Mcp-Session-Id", session["id"])
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, resp.headers, resp.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read()


def rpc(method, params=None):
    session["rid"] += 1
    body = {"jsonrpc": "2.0", "id": session["rid"], "method": method}
    if params is not None:
        body["params"] = params
    status, headers, raw = http("POST", "/mcp", body)
    if status != 200:
        raise SystemExit(f"{method}: HTTP {status}: {raw[:300]!r}")
    sid = headers.get("Mcp-Session-Id")
    if sid and not session["id"]:
        session["id"] = sid
    text = raw.decode()
    if headers.get("Content-Type", "").startswith("text/event-stream"):
        payload = [line[5:].strip() for line in text.splitlines() if line.startswith("data:")]
        text = payload[-1] if payload else "{}"
    message = json.loads(text)
    if "error" in message:
        raise SystemExit(f"{method}: {message['error']}")
    return message["result"]


def call(tool, arguments):
    result = rpc("tools/call", {"name": tool, "arguments": arguments})
    structured = result.get("structuredContent")
    if structured is None:
        structured = json.loads(result["content"][0]["text"])
    return result.get("isError", False), structured


failures = 0


def check(name, ok, detail=""):
    global failures
    print(f"{'ok  ' if ok else 'FAIL'} {name} {detail}".rstrip())
    if not ok:
        failures += 1


status, headers, _ = http("POST", "/mcp", {"jsonrpc": "2.0", "id": 0, "method": "ping"}, token=None)
www = headers.get("WWW-Authenticate", "")
check("unauthenticated refused", status == 401 and "resource_metadata=" in www, f"HTTP {status} {www}")

status, _, raw = http("GET", "/healthz", token=None)
health = json.loads(raw or b"{}")
check("healthz", status == 200 and health.get("ok") is True, json.dumps(health)[:200])

init = rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "hub-probe", "version": "0"}})
check("initialize", init["serverInfo"]["name"] == "devagotchi-hub", f"{init['serverInfo']} protocol {init['protocolVersion']}")
http("POST", "/mcp", {"jsonrpc": "2.0", "method": "notifications/initialized"})
if args.expect_error:
    is_error, answer = call("operation_get", {"broker": args.broker, "input": {"operation_id": "probe-" + uuid.uuid4().hex}})
    check(f"routed call fails with {args.expect_error}", is_error and answer.get("code") == args.expect_error, json.dumps(answer)[:200])
    sys.exit(1 if failures else 0)
listed = rpc("tools/list")["tools"]
tools = sorted(t["name"] for t in listed)
# Directory gate (Claude and OpenAI): every tool has a title and says whether it only reads
# or may destroy, and whether it reaches the open world.
unannotated = [t["name"] for t in listed if not t.get("title") or not (
    "readOnlyHint" in t.get("annotations", {}) and "openWorldHint" in t.get("annotations", {})
    and (t["annotations"]["readOnlyHint"] or "destructiveHint" in t["annotations"]))]
check("every tool has a title and annotations", not unannotated, ", ".join(unannotated))
broker_tools = ["approval_request", "approval_wait", "create_directory", "edit_file", "get_file_info",
                "list_directory", "machines", "move_file", "operation_cancel", "operation_get", "operation_read",
                "operation_start", "operation_wait", "read_file", "read_multiple_files", "run", "whoami", "write_file"]
board_tools = [t for t in tools if t.startswith("board_")]
check("tools", [t for t in tools if not t.startswith("board_")] == broker_tools and len(board_tools) == 18,
      ", ".join(tools))

is_error, me = call("whoami", {})
check("whoami", not is_error and args.broker in me.get("brokers", []), json.dumps(me))

is_error, seen = call("machines", {})
mine = [m for m in seen.get("machines", []) if m.get("name") == args.broker] if not is_error else []
check("machines lists the broker online", len(mine) == 1 and mine[0].get("status") == "online", json.dumps(seen)[:200])

is_error, started = call("operation_start", {"broker": args.broker, "input": {
    "idempotency_key": str(uuid.uuid4()), "executable": args.git, "args": ["--version"],
    "cwd": args.cwd, "deadline_ms": 30000}})
check("operation_start routed", not is_error, json.dumps(started)[:200])
operation = started.get("operation", {}).get("operation_id") if not is_error else None
state = None
if operation:
    # One held request through the hub: the broker answers when the operation settles.
    _, view = call("operation_wait", {"broker": args.broker, "input": {"operation_id": operation, "timeout_ms": 30000}})
    state = view.get("state")
    _, page = call("operation_read", {"broker": args.broker, "input": {"operation_id": operation, "cursor": 0, "max_bytes": 4096}})
    text = "".join(f.get("text", "") for f in page.get("frames", []))
    check("git --version under the broker", state == "succeeded" and "git version" in text, f"state={state} output={text.strip()[:60]!r}")

if args.run_broker:
    # One call: the command runs on the machine's shell (bash/sh or PowerShell) and its
    # output and exit code come back in the same answer.
    is_error, ran = call("run", {"machine": args.run_broker, "command": "echo devagotchi-run; exit 3"})
    check("run returns output and exit code in one call",
          not is_error and ran.get("state") == "failed" and ran.get("exit_code") == 3
          and "devagotchi-run" in ran.get("output", ""), json.dumps(ran)[:240])
    # Longer than the wait: still running, with how to follow it; then stopped explicitly.
    is_error, slow = call("run", {"machine": args.run_broker, "command": "sleep 20", "wait_seconds": 1})
    check("run hands back a still-running command", not is_error and slow.get("state") in ("accepted", "running")
          and "operation_read" in (slow.get("next") or ""), json.dumps(slow)[:240])
    if not is_error:
        # A waiter is woken by the cancellation itself, not by a timer.
        began = time.monotonic()
        call("operation_cancel", {"broker": args.run_broker, "input": {"operation_id": slow["operation_id"]}})
        _, stopped = call("operation_wait", {"broker": args.run_broker,
                                             "input": {"operation_id": slow["operation_id"], "timeout_ms": 30000}})
        check("operation_wait answers when a cancelled command settles",
              stopped.get("state") == "cancelled" and time.monotonic() - began < 15, json.dumps(stopped)[:200])
    # A broker that runs only granted programs refuses a command line, in words.
    is_error, refused = call("run", {"machine": args.broker, "command": "echo hi"})
    check("run on a granted-only broker refused", is_error and refused.get("code") == "command_not_allowed",
          json.dumps(refused)[:200])

if args.run_broker and args.run_dir:
    import os
    probe_file = os.path.join(args.run_dir, "devagotchi-probe.txt")
    with open(probe_file, "w", newline="\n") as handle:
        handle.write("devagotchi-file\n")
    is_error, content = call("read_file", {"machine": args.run_broker, "path": probe_file})
    check("read_file returns text in one call", not is_error and content.get("text") == "devagotchi-file\n"
          and content.get("eof") is True, json.dumps(content)[:200])
    is_error, tail = call("read_file", {"machine": args.run_broker, "path": probe_file, "offset": -5})
    check("read_file reads a tail", not is_error and tail.get("text") == "file\n", json.dumps(tail)[:200])
    is_error, listing = call("list_directory", {"machine": args.run_broker, "path": args.run_dir})
    names = {e.get("path"): e.get("kind") for e in listing.get("entries", [])} if not is_error else {}
    check("list_directory lists the file", names.get("devagotchi-probe.txt") == "file", json.dumps(listing)[:200])
    # Write, edit, read back; a stale checksum refuses an overwrite.
    written_file = os.path.join(args.run_dir, "devagotchi-written.txt")
    is_error, made = call("write_file", {"machine": args.run_broker, "path": written_file,
                                         "text": "alpha\nbeta\n", "mode": "create"})
    check("write_file creates", not is_error and made.get("created") is True and len(made.get("sha256", "")) == 64,
          json.dumps(made)[:200])
    is_error, edited = call("edit_file", {"machine": args.run_broker, "path": written_file,
                                          "old_text": "beta", "new_text": "gamma"})
    check("edit_file replaces exactly once", not is_error and edited.get("replacements") == 1, json.dumps(edited)[:200])
    is_error, back = call("read_file", {"machine": args.run_broker, "path": written_file})
    check("the edit reads back", not is_error and back.get("text") == "alpha\ngamma\n", json.dumps(back)[:200])
    is_error, stale = call("write_file", {"machine": args.run_broker, "path": written_file, "text": "lost",
                                          "expected_sha256": made.get("sha256", "")})
    check("a stale checksum refuses the overwrite", is_error and stale.get("code") == "changed_since_read",
          json.dumps(stale)[:200])
    is_error, outside_write = call("write_file", {"machine": args.broker, "path": written_file, "text": "x"})
    check("write_file outside a granted broker's workspace refused",
          is_error and outside_write.get("code") == "path_outside_workspace", json.dumps(outside_write)[:200])
    # A directory, a move, info and several files at once, on the machine.
    made_dir = os.path.join(args.run_dir, "probe-dir", "deeper")
    is_error, directory = call("create_directory", {"machine": args.run_broker, "path": made_dir})
    check("create_directory makes the parents", not is_error and directory.get("made") is True
          and directory.get("kind") == "dir", json.dumps(directory)[:200])
    moved_file = os.path.join(made_dir, "moved.txt")
    is_error, moved = call("move_file", {"machine": args.run_broker, "source": written_file,
                                         "destination": moved_file})
    check("move_file moves", not is_error and moved.get("replaced") is False and not os.path.exists(written_file),
          json.dumps(moved)[:200])
    is_error, again = call("move_file", {"machine": args.run_broker, "source": probe_file,
                                         "destination": moved_file})
    check("move_file never replaces without overwrite", is_error and again.get("code") == "already_exists",
          json.dumps(again)[:200])
    is_error, info = call("get_file_info", {"machine": args.run_broker, "path": moved_file})
    check("get_file_info counts lines", not is_error and info.get("kind") == "file" and info.get("lines") == 2,
          json.dumps(info)[:200])
    is_error, both = call("read_multiple_files", {"machine": args.run_broker,
                                                  "paths": [moved_file, os.path.join(args.run_dir, "missing.txt")]})
    files = both.get("files", []) if not is_error else []
    check("read_multiple_files reads one and reports the other", len(files) == 2
          and (files[0].get("content") or {}).get("text") == "alpha\ngamma\n"
          and (files[1].get("error") or {}).get("code") == "path_not_found", json.dumps(both)[:240])
    # A broker that runs only granted programs reaches files only inside its workspace.
    is_error, outside = call("read_file", {"machine": args.broker, "path": probe_file})
    check("read_file outside a granted broker's workspace refused",
          is_error and outside.get("code") == "path_outside_workspace", json.dumps(outside)[:200])

is_error, denied = call("operation_start", {"broker": args.ungranted, "input": {
    "idempotency_key": str(uuid.uuid4()), "executable": args.git, "args": ["--version"], "deadline_ms": 5000}})
check("ungranted broker refused", is_error and denied.get("code") in ("not_granted", "unknown_broker"), json.dumps(denied)[:160])

status, _, raw = http("GET", "/healthz", token=None)
health = json.loads(raw or b"{}")
chain = health.get("audit") or {}
check("audit chain verified", chain.get("ok") is True and chain.get("entries", 0) >= 4, json.dumps(chain))

sys.exit(1 if failures else 0)
