#!/usr/bin/env python3
"""Shippin (Nomen) sign-in end to end against the real hub binary and a fake Nomen.

    python3 tests/nomen_probe.py --devagotchi target/debug/devagotchi --endpoint <broker endpoint> \
        --git /usr/bin/git --cwd <workspace> --work <empty dir>

Starts tests/fake_nomen.py (which checks the hub's private-key JWT), then the hub in
`nomen` mode with an owner and one member grant, and checks: an email as owner stops
the hub at start; the owner reaches every broker without a grant and runs a real
operation (tests/hub_probe.py); a member reaches only its grant; tokens for another
project, from another issuer, issued to an application the hub does not accept,
inactive, naming an email or a padded account ID are refused with 401; a Nomen
outage answers 503 (never a refusal) while a session Nomen already confirmed keeps
working, and sign-in recovers when Nomen does. Then the terminal sign-in (D22, packet 14):
`devagotchi login` against the fake, with no browser — the code and URL are shown, the
tokens land owner-only in account.json, `whoami` names the account, the hub accepts the
token that landed, a `devagotchi mcp --hub` session on stdio is answered as the account
(an agent's call carries the account), the command line bound to the hub works one board
from two machines of the account (B11), and `logout` revokes it. Exit 0 only when every
step holds.
"""
import argparse
import json
import os
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.dont_write_bytecode = True  # importing fake_nomen leaves no cache in the tree
sys.path.insert(0, str(HERE))
from fake_nomen import FakeNomen  # noqa: E402

parser = argparse.ArgumentParser()
parser.add_argument("--devagotchi", required=True, help="the one binary; the hub is `devagotchi hub serve`")
parser.add_argument("--endpoint", required=True, help="the running broker's endpoint")
parser.add_argument("--git", required=True)
parser.add_argument("--cwd", required=True)
parser.add_argument("--work", required=True, type=Path)
args = parser.parse_args()

OWNER, MEMBER = "300000000000000001", "300000000000000002"
HUB_PROJECT = "300000000000000500"
failures = 0


def check(name, ok, detail=""):
    global failures
    print(f"{'ok  ' if ok else 'FAIL'} {name} {detail}".rstrip(), flush=True)
    if not ok:
        failures += 1


class Mcp:
    def __init__(self, base, token):
        self.base, self.token, self.session, self.rid = base, token, None, 0

    def post(self, body):
        req = urllib.request.Request(self.base + "/mcp", data=json.dumps(body).encode(), method="POST")
        req.add_header("Accept", "application/json, text/event-stream")
        req.add_header("Content-Type", "application/json")
        req.add_header("MCP-Protocol-Version", "2025-06-18")
        req.add_header("Authorization", "Bearer " + self.token)
        if self.session:
            req.add_header("Mcp-Session-Id", self.session)
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return resp.status, resp.headers, resp.read()
        except urllib.error.HTTPError as error:
            return error.code, error.headers, error.read()

    def rpc(self, method, params=None):
        self.rid += 1
        status, headers, raw = self.post({"jsonrpc": "2.0", "id": self.rid, "method": method, "params": params or {}})
        if status != 200:
            return status, headers, None
        self.session = self.session or headers.get("Mcp-Session-Id")
        text = raw.decode()
        if headers.get("Content-Type", "").startswith("text/event-stream"):
            data = [line[5:].strip() for line in text.splitlines() if line.startswith("data:")]
            text = data[-1] if data else "{}"
        return status, headers, json.loads(text).get("result")

    def open(self):
        status, headers, _ = self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                                     "clientInfo": {"name": "nomen-probe", "version": "0"}})
        if status == 200:
            self.post({"jsonrpc": "2.0", "method": "notifications/initialized"})
        return status, headers

    def tool(self, name, arguments):
        _, _, result = self.rpc("tools/call", {"name": name, "arguments": arguments})
        structured = result.get("structuredContent")
        if structured is None:
            structured = json.loads(result["content"][0]["text"])
        return result.get("isError", False), structured


def whoami(base, token):
    client = Mcp(base, token)
    status, headers = client.open()
    if status != 200:
        return status, headers.get("WWW-Authenticate", ""), None
    return status, "", client.tool("whoami", {})[1]


def start_hub(config_path, state):
    process = subprocess.Popen([args.devagotchi, "hub", "serve", "--state-dir", str(state), "--config", str(config_path)],
                               stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    lines, ready = [], threading.Event()

    def pump():
        for line in process.stderr:
            print(f"  [hub] {line.rstrip()}", flush=True)
            lines.append(line)
            if "ready:" in line:
                ready.set()
        ready.set()

    threading.Thread(target=pump, daemon=True).start()
    ready.wait(30)
    return process, lines


nomen = FakeNomen()
args.work.mkdir(parents=True, exist_ok=True)
key_file = args.work / "hub-key.json"
key_file.write_text(nomen.key_file())
if os.name != "nt":
    key_file.chmod(0o600)
with socket.socket() as s:
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
base = f"http://127.0.0.1:{port}"
config = {
    "listen": f"127.0.0.1:{port}",
    "public_url": base,
    "auth": {"mode": "nomen", "issuer": nomen.issuer, "audiences": [HUB_PROJECT],
             "clients": ["shippin-mcp", "devagotchi-cli@shippin"], "key_file": str(key_file)},
    "account_boards": {},
    "owner": OWNER,
    "brokers": [{"name": "local", "socket": args.endpoint}, {"name": "elsewhere", "link": True}],
    "grants": [{"subject": MEMBER, "brokers": ["local"]}],
}

# An email where the account ID belongs stops the hub before it serves anything.
bad = args.work / "hub-email-owner.json"
bad.write_text(json.dumps({**config, "owner": "jessay@example.test"}))
refused = subprocess.run([args.devagotchi, "hub", "serve", "--state-dir", str(args.work / "unused"), "--config", str(bad)],
                         capture_output=True, text=True, timeout=30, stdin=subprocess.DEVNULL)
check("email as owner refused at start", refused.returncode != 0 and "account ID" in refused.stderr,
      refused.stderr.strip()[:160])

config_path = args.work / "hub-nomen.json"
config_path.write_text(json.dumps(config))
hub, lines = start_hub(config_path, args.work / "hub-state")
try:
    check("hub ready in nomen mode", any(f"nomen {nomen.issuer}" in line for line in lines), "")
    check("owner named at start", any(OWNER in line and "owner" in line for line in lines), "")

    nomen.answers.update({
        "tok-owner": nomen.active(OWNER, [HUB_PROJECT, "300000000000000001"], username="jessay@example.test"),
        "tok-owner-2": nomen.active(OWNER, HUB_PROJECT, username="jessay@example.test"),
        "tok-member": nomen.active(MEMBER, [HUB_PROJECT]),
        "tok-member-2": nomen.active(MEMBER, [HUB_PROJECT]),
        "tok-other-project": nomen.active(OWNER, ["300000000000000777"]),
        "tok-other-issuer": nomen.active(OWNER, [HUB_PROJECT], iss="https://id.elsewhere.example"),
        "tok-email-sub": nomen.active("jessay@example.test", [HUB_PROJECT]),
        "tok-expired": nomen.active(OWNER, [HUB_PROJECT], exp=int(time.time()) - 600),
        "tok-tenant-app": nomen.active(OWNER, [HUB_PROJECT], client_id="tenant-app"),
        "tok-padded-sub": nomen.active(" " + OWNER, [HUB_PROJECT]),
    })

    with urllib.request.urlopen(base + "/.well-known/oauth-protected-resource", timeout=10) as resp:
        meta = json.loads(resp.read())
    check("protected resource names Nomen", meta.get("authorization_servers") == [nomen.issuer], json.dumps(meta))

    status, _, me = whoami(base, "tok-owner")
    check("owner whoami", status == 200 and me and me.get("subject") == OWNER and me.get("owner") is True
          and me.get("via") == "nomen" and me.get("name") == "jessay@example.test"
          and me.get("brokers") == ["local", "elsewhere"], json.dumps(me))

    status, _, me = whoami(base, "tok-member")
    check("member whoami", status == 200 and me and me.get("owner") is False and me.get("brokers") == ["local"],
          json.dumps(me))
    member = Mcp(base, "tok-member")
    member.open()
    is_error, answer = member.tool("operation_get", {"broker": "elsewhere", "input": {"operation_id": "x"}})
    check("member refused outside its grant", is_error and answer.get("code") == "not_granted", json.dumps(answer))

    for token in ("tok-other-project", "tok-other-issuer", "tok-email-sub", "tok-expired", "tok-never-issued",
                  "tok-tenant-app", "tok-padded-sub"):
        status, www, _ = whoami(base, token)
        check(f"{token} refused", status == 401 and 'error="invalid_token"' in www, f"HTTP {status} {www}")

    print("--- owner runs a real operation through the hub", flush=True)
    probe = subprocess.run([sys.executable, str(HERE / "hub_probe.py"), base, "--token", "tok-owner",
                            "--broker", "local", "--git", args.git, "--cwd", args.cwd], timeout=120)
    check("owner operation via hub_probe", probe.returncode == 0, f"exit {probe.returncode}")

    # Nomen goes down: a session Nomen confirmed moments ago keeps working; a new one
    # waits for Nomen (503), and is never told it was refused.
    status, _, _ = whoami(base, "tok-owner-2")
    nomen.status = 503
    status_cached, _, me = whoami(base, "tok-owner-2")
    check("confirmed session survives a Nomen outage", status_cached == 200 and me and me.get("owner") is True,
          f"HTTP {status_cached}")
    status_new, _, _ = whoami(base, "tok-member-2")
    check("new sign-in during the outage is 503, not 401", status_new == 503, f"HTTP {status_new}")
    nomen.status = 200
    status_back, _, me = whoami(base, "tok-member-2")
    check("sign-in recovers with Nomen", status_back == 200 and me and me.get("subject") == MEMBER,
          f"HTTP {status_back}")

    print("--- terminal sign-in: devagotchi login against the fake, no browser", flush=True)
    cli_config = args.work / "cli-config"
    env = {**os.environ, "DEVAGOTCHI_CONFIG_DIR": str(cli_config)}
    nomen.userinfo["tok-device"] = {"sub": MEMBER, "preferred_username": "member", "email": "member@example.test"}
    nomen.answers["tok-device"] = nomen.active(MEMBER, [HUB_PROJECT], client_id="devagotchi-cli@shippin",
                                               username="member")
    before = subprocess.run([args.devagotchi, "whoami"], capture_output=True, text=True, timeout=30, env=env)
    check("whoami before: not signed in", before.returncode == 0 and "not signed in" in before.stdout,
          before.stdout.strip())
    login = subprocess.run([args.devagotchi, "login", "--issuer", nomen.issuer, "--client-id", "devagotchi-cli@shippin"],
                           capture_output=True, text=True, timeout=60, env=env, stdin=subprocess.DEVNULL)
    check("login shows the code and the URL, then signs in", login.returncode == 0 and "WXYZ-1234" in login.stderr
          and f"{nomen.issuer}/device" in login.stderr and f"account {MEMBER}" in login.stderr,
          login.stderr.strip().replace("\n", " | ")[:300])
    account_file = cli_config / "account.json"
    stored = json.loads(account_file.read_text()) if account_file.is_file() else {}
    check("account.json holds the account", stored.get("kind") == "account" and stored.get("subject") == MEMBER
          and stored.get("access_token") == "tok-device" and stored.get("username") == "member", json.dumps(stored)[:200])
    if os.name != "nt":
        check("account.json is owner-only", (account_file.stat().st_mode & 0o777) == 0o600,
              oct(account_file.stat().st_mode & 0o777))
    me_cli = subprocess.run([args.devagotchi, "whoami", "--json"], capture_output=True, text=True, timeout=30, env=env)
    me_json = json.loads(me_cli.stdout or "{}")
    check("whoami names the account", me_cli.returncode == 0 and me_json.get("signed_in") is True
          and me_json.get("subject") == MEMBER and me_json.get("standing") == "fresh", me_cli.stdout.strip())
    # The token that landed is a token the hub accepts: an agent's call carries the account.
    status, _, me = whoami(base, stored.get("access_token", ""))
    check("the hub accepts the token login landed", status == 200 and me and me.get("subject") == MEMBER
          and me.get("via") == "nomen" and me.get("brokers") == ["local"], json.dumps(me))
    # An agent's MCP call carries the account: `devagotchi mcp --hub` on stdio, the hub's
    # whoami answering as the signed-in account, the token never leaving this machine.
    front = subprocess.Popen([args.devagotchi, "mcp", "--hub", base], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, text=True, bufsize=1, env=env)
    def ask(rid, method, params):
        front.stdin.write(json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}) + "\n")
        front.stdin.flush()
        while True:
            line = front.stdout.readline()
            if not line:
                return None
            reply = json.loads(line)
            if reply.get("id") == rid:
                return reply
    init = ask(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "headless", "version": "0"}})
    front.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n"); front.stdin.flush()
    me_front = ask(2, "tools/call", {"name": "whoami", "arguments": {}})
    front.stdin.close()
    front.wait(10)
    content = (me_front or {}).get("result", {}).get("structuredContent") or {}
    check("devagotchi mcp --hub: the hub's whoami answers as the account", init and init.get("result", {}).get("serverInfo", {}).get("name") == "devagotchi-hub"
          and content.get("subject") == MEMBER and content.get("via") == "nomen" and front.returncode == 0,
          json.dumps(content) or front.stderr.read()[:200])
    # The command line bound to the hub (B11): two machines of one account, one board.
    machines = [args.work / "machine-a" / "demo", args.work / "machine-b" / "demo"]
    for m in machines:
        m.mkdir(parents=True, exist_ok=True)
    def board(machine, actor, *words):
        e = {**env, "DEVAGOTCHI_ACTOR": actor} if actor else {k: v for k, v in env.items() if k != "DEVAGOTCHI_ACTOR"}
        r = subprocess.run([args.devagotchi, "board", *words, "--json"], capture_output=True, text=True, timeout=60, env=e, cwd=machine)
        try:
            return r.returncode, json.loads(r.stdout or "null")
        except json.JSONDecodeError:
            return r.returncode, {"stdout": r.stdout[:200], "stderr": r.stderr[:200]}
    code, bound = board(machines[0], None, "bind", "--hub", base)
    check("machine A binds the project to the hub", code == 0 and bound.get("created") is True and bound.get("hub") == base, json.dumps(bound)[:200])
    code, made = board(machines[0], "claude", "new", "Prove the bound command line")
    code2, lease = board(machines[0], "claude", "claim", made.get("id", "?"))
    check("machine A's seat claude claims on the hub", code == 0 and code2 == 0 and lease.get("ticket", {}).get("holder") == "claude", json.dumps(lease)[:160])
    code, bound_b = board(machines[1], None, "bind", "--hub", base, "--project", "demo")
    code2, prime = board(machines[1], "codex", "prime")
    check("machine B binds the same board and sees the claim at once", code == 0 and bound_b.get("created") is False
          and code2 == 0 and prime.get("actor") == "codex" and prime.get("ready") == [], json.dumps(prime)[:160])
    check("no local store is written for a bound project", not (machines[0] / ".devagotchi" / "board.sqlite3").exists()
          and not (machines[1] / ".devagotchi" / "board.sqlite3").exists())
    logout = subprocess.run([args.devagotchi, "logout"], capture_output=True, text=True, timeout=30, env=env)
    check("logout revokes both tokens and removes the file", logout.returncode == 0
          and nomen.revoked == ["tok-device", "rt-device"] and not account_file.exists(),
          f"{logout.stderr.strip()} revoked={nomen.revoked}")
    local = subprocess.run([args.devagotchi, "login", "--local"], capture_output=True, text=True, timeout=30, env=env)
    after = subprocess.run([args.devagotchi, "whoami"], capture_output=True, text=True, timeout=30, env=env)
    check("login --local is the accountless choice, on file", local.returncode == 0 and "runs locally" in after.stdout,
          after.stdout.strip())
finally:
    hub.terminate()
    try:
        hub.wait(10)
    except subprocess.TimeoutExpired:
        hub.kill()
    nomen.close()

print("NOMEN PROBE PASSED" if not failures else f"NOMEN PROBE FAILED ({failures})", flush=True)
sys.exit(1 if failures else 0)
