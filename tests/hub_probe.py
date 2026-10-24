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
broker_tools = ["approval_request", "approval_wait", "machines", "operation_cancel", "operation_get", "operation_read", "operation_start", "whoami"]
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
    for _ in range(300):  # up to 30 s
        _, view = call("operation_get", {"broker": args.broker, "input": {"operation_id": operation}})
        state = view.get("state")
        # Non-terminal states per OperationState::terminal (contracts): accepted, running.
        if state not in ("accepted", "running"):
            break
        time.sleep(0.1)
    _, page = call("operation_read", {"broker": args.broker, "input": {"operation_id": operation, "cursor": 0, "max_bytes": 4096}})
    text = "".join(f.get("text", "") for f in page.get("frames", []))
    check("git --version under the broker", state == "succeeded" and "git version" in text, f"state={state} output={text.strip()[:60]!r}")

is_error, denied = call("operation_start", {"broker": args.ungranted, "input": {
    "idempotency_key": str(uuid.uuid4()), "executable": args.git, "args": ["--version"], "deadline_ms": 5000}})
check("ungranted broker refused", is_error and denied.get("code") in ("not_granted", "unknown_broker"), json.dumps(denied)[:160])

status, _, raw = http("GET", "/healthz", token=None)
health = json.loads(raw or b"{}")
chain = health.get("audit") or {}
check("audit chain verified", chain.get("ok") is True and chain.get("entries", 0) >= 4, json.dumps(chain))

sys.exit(1 if failures else 0)
