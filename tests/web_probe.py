#!/usr/bin/env python3
"""Browser sign-in end to end: the real hub binary in `nomen` mode, a fake Nomen.

    python3 tests/web_probe.py --devagotchi target/debug/devagotchi --endpoint <broker endpoint> \
        --work <empty dir>

Plays a browser with no redirect following: /account sends a signed-out visitor to
sign in; /auth/login sends them to Nomen with PKCE; the probe plays Nomen's
authorization step (a code bound to the challenge); /auth/callback trades the code at
the fake token endpoint and the hub checks the token with real Nomen introspection
(private-key JWT). Then: the account page and /api/v1/me show the person and their
machines (the running broker online, an unenrolled linked one), a token issued to an
application the hub does not accept is refused (403, no session), a callback while
Nomen is down is 503 while a confirmed session keeps its page, and sign-out ends the
session. Exit 0 only when every step holds.
"""
import argparse
import http.client
import json
import os
import socket
import subprocess
import sys
import threading
import urllib.parse
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.dont_write_bytecode = True
sys.path.insert(0, str(HERE))
from fake_nomen import FakeNomen  # noqa: E402

parser = argparse.ArgumentParser()
parser.add_argument("--devagotchi", required=True, help="the one binary; the hub is `devagotchi hub serve`")
parser.add_argument("--endpoint", required=True, help="the running broker's endpoint")
parser.add_argument("--work", required=True, type=Path)
args = parser.parse_args()

OWNER = "300000000000000001"
HUB_PROJECT = "300000000000000500"
WEB = "devagotchi-web"
failures = 0


def check(name, ok, detail=""):
    global failures
    print(f"{'ok  ' if ok else 'FAIL'} {name} {detail}".rstrip(), flush=True)
    if not ok:
        failures += 1


class Browser:
    """Cookies by hand, redirects never followed, so every hop is checked."""

    def __init__(self, port):
        self.port, self.cookies = port, {}

    def request(self, method, path):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=30)
        headers = {}
        if self.cookies:
            headers["Cookie"] = "; ".join(f"{k}={v}" for k, v in self.cookies.items())
        conn.request(method, path, headers=headers)
        resp = conn.getresponse()
        body = resp.read().decode(errors="replace")
        for value in resp.headers.get_all("Set-Cookie") or []:
            pair, attrs = value.split(";", 1) if ";" in value else (value, "")
            name, _, val = pair.partition("=")
            if "Max-Age=0" in attrs:
                self.cookies.pop(name, None)
            else:
                self.cookies[name] = val
        conn.close()
        return resp.status, resp.headers, body

    def sign_in(self, nomen, token, redirect_uri):
        """Returns (status, headers, body) of the callback."""
        status, headers, _ = self.request("GET", "/auth/login?return_to=/account")
        to = headers.get("Location", "")
        params = dict(urllib.parse.parse_qsl(urllib.parse.urlsplit(to).query))
        ok = (status == 303 and to.startswith(nomen.issuer + "/oauth/v2/authorize?")
              and params.get("client_id") == WEB and params.get("code_challenge_method") == "S256"
              and params.get("redirect_uri") == redirect_uri
              and f"urn:nomen:iam:org:project:id:{HUB_PROJECT}:aud" in params.get("scope", "")
              and "dg_login" in self.cookies)
        check("login sends the browser to Nomen with PKCE", ok, f"HTTP {status} {to[:120]}")
        code = nomen.issue_code(params.get("code_challenge", ""), token, WEB, redirect_uri)
        query = urllib.parse.urlencode({"code": code, "state": params.get("state", "")})
        return self.request("GET", f"/auth/callback?{query}")


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
redirect_uri = base + "/auth/callback"
config = {
    "listen": f"127.0.0.1:{port}",
    "public_url": base,
    "auth": {"mode": "nomen", "issuer": nomen.issuer, "audiences": [HUB_PROJECT], "clients": ["shippin-mcp", WEB],
             "key_file": str(key_file)},
    "owner": OWNER,
    "web": {"client_id": WEB},
    "brokers": [{"name": "local", "socket": args.endpoint}, {"name": "elsewhere", "link": True}],
    "grants": [],
}
config_path = args.work / "hub-web.json"
config_path.write_text(json.dumps(config))
nomen.answers.update({
    "tok-web": nomen.active(OWNER, [HUB_PROJECT], username="owner@example.test", client_id=WEB),
    "tok-web-2": nomen.active(OWNER, [HUB_PROJECT], username="owner@example.test", client_id=WEB),
    "tok-foreign-app": nomen.active(OWNER, [HUB_PROJECT], client_id="tenant-app"),
})
hub, lines = start_hub(config_path, args.work / "hub-state")
try:
    check("hub announces web sign-in", any(f"web sign-in: {base}/account" in line for line in lines))
    browser = Browser(port)
    status, headers, _ = browser.request("GET", "/account")
    check("signed-out visitor is sent to sign in", status == 303
          and headers.get("Location") == "/auth/login?return_to=/account", f"HTTP {status}")

    status, headers, _ = browser.sign_in(nomen, "tok-web", redirect_uri)
    check("callback opens a session and returns to the account", status == 303
          and headers.get("Location") == "/account" and "dg_session" in browser.cookies
          and "dg_login" not in browser.cookies, f"HTTP {status} {sorted(browser.cookies)}")
    check("the browser never holds the Nomen token", "tok-web" not in json.dumps(browser.cookies))

    status, headers, page = browser.request("GET", "/account")
    check("account page shows the person", status == 200 and OWNER in page and "owner@example.test" in page
          and "You own this hub" in page, f"HTTP {status}")
    check("account page lists machines with live status", "<td>local</td>" in page and "status online" in page
          and "<td>elsewhere</td>" in page and "status not_enrolled" in page)
    check("account page has a strict CSP", headers.get("Content-Security-Policy", "").startswith("default-src 'none'"))

    status, _, body = browser.request("GET", "/api/v1/me")
    me = json.loads(body) if status == 200 else {}
    check("/api/v1/me by session", me.get("subject") == OWNER and me.get("via") == "nomen"
          and me.get("name") == "owner@example.test"
          and [(m["name"], m["status"]) for m in me.get("machines", [])]
          == [("local", "online"), ("elsewhere", "not_enrolled")], body[:300])

    stranger = Browser(port)
    status, _, page = stranger.sign_in(nomen, "tok-foreign-app", redirect_uri)
    check("a token from an application the hub does not accept is refused", status == 403
          and "dg_session" not in stranger.cookies, f"HTTP {status}")

    nomen.status = 503
    status, _, _ = browser.request("GET", "/account")
    check("a confirmed session keeps its page while Nomen is down", status == 200, f"HTTP {status}")
    late = Browser(port)
    status, _, _ = late.sign_in(nomen, "tok-web-2", redirect_uri)
    check("a sign-in while Nomen is down is 503, never a refusal", status == 503
          and "dg_session" not in late.cookies, f"HTTP {status}")
    nomen.status = 200

    old = dict(browser.cookies)
    status, headers, _ = browser.request("POST", "/auth/logout")
    check("sign-out ends the session", status == 303 and headers.get("Location") == "/signed-out"
          and "dg_session" not in browser.cookies, f"HTTP {status}")
    browser.cookies = old
    status, _, _ = browser.request("GET", "/account")
    check("the old session id no longer works", status == 303, f"HTTP {status}")
finally:
    hub.terminate()
    try:
        hub.wait(10)
    except subprocess.TimeoutExpired:
        hub.kill()
    nomen.close()

print("WEB PROBE PASSED" if not failures else f"WEB PROBE FAILED ({failures})", flush=True)
sys.exit(1 if failures else 0)
