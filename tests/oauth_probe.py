#!/usr/bin/env python3
"""MCP OAuth against the real hub binary, the way claude.ai connects.

Starts two fake OpenID providers and a hub in auth.mode devagotchi (no Nomen
key, no project id). Then: dynamic client registration, authorization code +
PKCE through the first provider, token, MCP initialize and whoami. A second
call with the same subject is the same Devagotchi account. A second provider
with the same verified email links to that account. An unverified email does not.

    python3 tests/oauth_probe.py --devagotchi target/debug/devagotchi --work /tmp/oauth
"""

import argparse
import base64
import hashlib
import json
import os
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--devagotchi", required=True)
parser.add_argument("--work", required=True, type=Path)
args = parser.parse_args()
failures = 0
LAST_WHOAMI = ""


def check(name, ok, detail=""):
    global failures
    print(f"{'ok  ' if ok else 'FAIL'} {name} {detail}".rstrip(), flush=True)
    if not ok:
        failures += 1


class Provider(threading.Thread):
    def __init__(self, sub, email, verified):
        super().__init__(daemon=True)
        self.sub, self.email, self.verified = sub, email, verified
        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), self._handler())
        self.issuer = f"http://127.0.0.1:{self.httpd.server_address[1]}"

    def _handler(self):
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                return

            def _json(self, status, body):
                raw = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_GET(self):
                path = urllib.parse.urlparse(self.path).path
                if path == "/.well-known/openid-configuration":
                    base = outer.issuer
                    self._json(200, {
                        "issuer": base,
                        "authorization_endpoint": base + "/authorize",
                        "token_endpoint": base + "/token",
                        "userinfo_endpoint": base + "/userinfo",
                    })
                    return
                if path == "/authorize":
                    query = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
                    redirect = query.get("redirect_uri", [""])[0]
                    state = query.get("state", [""])[0]
                    location = redirect + ("&" if "?" in redirect else "?") + urllib.parse.urlencode(
                        {"code": "provider-code", "state": state}
                    )
                    self.send_response(302)
                    self.send_header("Location", location)
                    self.end_headers()
                    return
                if path == "/userinfo":
                    self._json(200, {
                        "sub": outer.sub,
                        "email": outer.email,
                        "email_verified": outer.verified,
                        "name": "Ada",
                    })
                    return
                self._json(404, {"error": "not_found"})

            def do_POST(self):
                if urllib.parse.urlparse(self.path).path == "/token":
                    self._json(200, {"access_token": "provider-access", "token_type": "Bearer", "expires_in": 3600})
                    return
                self._json(404, {"error": "not_found"})

        return Handler

    def run(self):
        self.httpd.serve_forever()


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def start_hub(binary, config, state):
    process = subprocess.Popen(
        [binary, "hub", "serve", "--state-dir", str(state), "--config", str(config)],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
    )
    ready = threading.Event()

    def pump():
        for line in process.stderr:
            print(f"  [hub] {line.rstrip()}", flush=True)
            if "ready:" in line:
                ready.set()
        ready.set()

    threading.Thread(target=pump, daemon=True).start()
    ready.wait(30)
    return process


def request(opener, method, url, data=None, headers=None, token=None):
    req = urllib.request.Request(url, data=data, method=method)
    for key, value in (headers or {}).items():
        req.add_header(key, value)
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with opener.open(req, timeout=20) as resp:
            return resp.status, dict(resp.headers), resp.read()
    except urllib.error.HTTPError as error:
        return error.code, dict(error.headers), error.read()


def mcp(opener, base, token, method, params=None, rid=1, session=None):
    headers = {
        "Accept": "application/json, text/event-stream",
        "Content-Type": "application/json",
        "MCP-Protocol-Version": "2025-06-18",
    }
    if session:
        headers["Mcp-Session-Id"] = session
    body = json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}}).encode()
    status, response_headers, raw = request(opener, "POST", base + "/mcp", body, headers, token)
    session_id = response_headers.get("Mcp-Session-Id") or response_headers.get("mcp-session-id") or session
    return status, raw, session_id


def connect(opener, base, provider_name):
    verifier = "a" * 43
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    query = urllib.parse.urlencode({
        "response_type": "code",
        "client_id": CLIENT,
        "redirect_uri": REDIRECT,
        "code_challenge": challenge,
        "code_challenge_method": "S256",
        "state": "st",
        "scope": "openid email mcp",
    })
    status, _, raw = request(opener, "GET", base + "/authorize?" + query)
    text = raw.decode()
    check("authorize offers one tap", status == 200 and "/authorize/provisional" in text, f"HTTP {status} {text[:120]}")
    resume = text.split('name="request" value="')[1].split('"')[0]
    # Do not follow the provider redirect automatically: we want the code on our redirect.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *_args, **_kwargs):
            return None
    manual = urllib.request.build_opener(NoRedirect, urllib.request.HTTPCookieProcessor(opener.handlers[-1].cookiejar if False else urllib.request.HTTPCookieProcessor().cookiejar))
    # Use the same cookie jar as opener.
    jar = None
    for handler in opener.handlers:
        if isinstance(handler, urllib.request.HTTPCookieProcessor):
            jar = handler.cookiejar
    manual = urllib.request.build_opener(NoRedirect, urllib.request.HTTPCookieProcessor(jar))
    status, headers, _ = request(manual, "GET", f"{base}/login/upstream/{provider_name}?resume={urllib.parse.quote(resume)}")
    # The first hop is the provider. Follow it ourselves once, then the callback.
    location = headers.get("Location") or headers.get("location")
    check("upstream redirect", status in (302, 303) and location, f"HTTP {status} {location}")
    status, headers, _ = request(manual, "GET", location)
    location = headers.get("Location") or headers.get("location")
    status, headers, body = request(manual, "GET", location)
    location = headers.get("Location") or headers.get("location") or ""
    check("back from the provider", "authorize/resume" in location or status == 200, f"HTTP {status} {location[:120]}")
    if "authorize/resume" in location:
        status, _, body = request(opener, "GET", location if location.startswith("http") else base + location)
    consent = body.decode()
    check("consent names the client", "Allow" in consent, consent[:160])
    status, headers, _ = request(
        manual, "POST", base + "/authorize/consent",
        urllib.parse.urlencode({"request": resume, "decision": "allow"}).encode(),
        {"Content-Type": "application/x-www-form-urlencoded"},
    )
    location = headers.get("Location") or headers.get("location") or ""
    check("consent redirects with a code", "code=" in location, f"HTTP {status} {location[:160]}")
    code = urllib.parse.parse_qs(urllib.parse.urlparse(location).query)["code"][0]
    status, _, raw = request(
        opener, "POST", base + "/token",
        urllib.parse.urlencode({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": REDIRECT,
            "client_id": CLIENT,
            "code_verifier": verifier,
        }).encode(),
        {"Content-Type": "application/x-www-form-urlencoded"},
    )
    token = json.loads(raw).get("access_token") if status == 200 else None
    check("token", status == 200 and token, f"HTTP {status} {raw[:160]}")
    return token


def claim_via(base, provider, code):
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *_args, **_kwargs):
            return None
    jar = urllib.request.HTTPCookieProcessor()
    manual = urllib.request.build_opener(NoRedirect, jar)
    status, headers, _ = request(
        manual, "GET", f"{base}/login/upstream/{provider}?resume=claim:{urllib.parse.quote(code)}",
    )
    location = headers.get("Location") or headers.get("location")
    check("claim starts upstream", status in (302, 303) and location, f"HTTP {status}")
    if not location:
        return
    status, headers, _ = request(manual, "GET", location)
    location = headers.get("Location") or headers.get("location")
    if not location:
        check("claim provider redirected", False, f"HTTP {status}")
        return
    status, headers, _ = request(manual, "GET", location)
    check("claim finished", status in (200, 302, 303), f"HTTP {status}")


def whoami_subject(opener, base, token):
    status, raw, session = mcp(opener, base, token, "initialize", {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "oauth-probe", "version": "0"},
    })
    check("mcp initialize", status == 200 and b"devagotchi" in raw.lower(), f"HTTP {status} {raw[:120]}")
    status, raw, _session = mcp(
        opener, base, token, "tools/call", {"name": "whoami", "arguments": {}}, rid=2, session=session,
    )
    text = raw.decode()
    global LAST_WHOAMI
    LAST_WHOAMI = text
    check("whoami", status == 200 and "dg_" in text, text[:240])
    start = text.find("dg_")
    if start < 0:
        return ""
    return text[start:].split('"')[0].split("\\")[0]


args.work.mkdir(parents=True, exist_ok=True)
one = Provider("person-1", "ada@example.com", True)
two = Provider("person-2", "ada@example.com", True)
plain = Provider("person-3", "ada@example.com", False)
for provider in (one, two, plain):
    provider.start()

port = free_port()
base = f"http://127.0.0.1:{port}"
REDIRECT = "http://127.0.0.1:9/cb"
config = {
    "listen": f"127.0.0.1:{port}",
    "public_url": base,
    "auth": {
        "mode": "devagotchi",
        "local": {"magic_link": True, "deliver": "log"},
        "upstream": [
            {"name": "one", "issuer": one.issuer, "client_id": "hub"},
            {"name": "two", "issuer": two.issuer, "client_id": "hub"},
            {"name": "plain", "issuer": plain.issuer, "client_id": "hub"},
        ],
    },
    "brokers": [],
    "grants": [],
}
config_path = args.work / "hub.json"
config_path.write_text(json.dumps(config))
hub = start_hub(args.devagotchi, config_path, args.work / "state")
try:
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor())
    status, _, raw = request(opener, "GET", base + "/healthz")
    health = json.loads(raw)
    check("healthz auth.ready", status == 200 and health.get("auth", {}).get("ready") is True, json.dumps(health.get("auth"))[:300])
    status, _, raw = request(opener, "GET", base + "/.well-known/oauth-protected-resource/mcp")
    meta = json.loads(raw)
    check(
        "protected resource is the mcp url and this hub",
        meta.get("resource") == base + "/mcp" and meta.get("authorization_servers") == [base],
        json.dumps(meta)[:240],
    )
    status, _, raw = request(
        opener, "POST", base + "/register",
        json.dumps({
            "client_name": "Claude",
            "redirect_uris": [REDIRECT],
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        }).encode(),
        {"Content-Type": "application/json"},
    )
    CLIENT = json.loads(raw).get("client_id") if status == 201 else None
    check("dynamic registration", status == 201 and CLIENT, f"HTTP {status} {raw[:160]}")

    status, _, raw = request(
        opener, "POST", base + "/auth/agent/start",
        urllib.parse.urlencode({"client_id": "devagotchi-cli", "agent": "claude"}).encode(),
        {"Content-Type": "application/x-www-form-urlencoded"},
    )
    started = json.loads(raw) if status == 200 else {}
    agent_token = started.get("access_token")
    claim_code = started.get("claim_code")
    check("agent opens an account with no human", status == 200 and agent_token and claim_code, f"HTTP {status} {raw[:180]}")
    agent_id = whoami_subject(opener, base, agent_token) if agent_token else ""
    check(
        "whoami is provisional and shows the claim code",
        agent_id.startswith("dg_") and claim_code and claim_code in LAST_WHOAMI and "provisional_until" in LAST_WHOAMI,
        LAST_WHOAMI[:240],
    )
    status, _, session = mcp(opener, base, agent_token, "initialize", {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "oauth-probe", "version": "0"},
    })
    status, added, _session = mcp(
        opener, base, agent_token, "tools/call",
        {"name": "machine_add", "arguments": {"name": "laptop"}}, rid=3, session=session,
    )
    added_text = added.decode()
    check("machine_add returns the join line", status == 200 and "/join/" in added_text and "laptop" in added_text, added_text[:240])
    code = ""
    if '"code":"' in added_text:
        code = added_text.split('"code":"')[1].split('"')[0]
    elif '\\"code\\":\\"' in added_text:
        code = added_text.split('\\"code\\":\\"')[1].split('\\"')[0]
    status, _, enroll_raw = request(
        opener, "POST", base + "/enroll",
        json.dumps({"name": "laptop", "code": code, "public_key": "aa"}).encode(),
        {"Content-Type": "application/json"},
    )
    check("enroll binds the machine", status == 201, f"HTTP {status} {enroll_raw[:160]}")
    status, op_raw, _ = mcp(
        opener, base, agent_token, "tools/call",
        {"name": "operation_get", "arguments": {"broker": "laptop", "input": {"operation_id": "x"}}},
        rid=4, session=session,
    )
    op_text = op_raw.decode()
    check("owned machine is routed", "not_granted" not in op_text and "unknown_broker" not in op_text, op_text[:240])

    saved = (one.sub, one.email, one.verified)
    one.sub, one.email, one.verified = "claimer", "fresh@example.com", True
    claim_via(base, "one", claim_code or "")
    claimed_id = whoami_subject(opener, base, agent_token) if agent_token else ""
    check("claim keeps the account", claimed_id == agent_id and "provisional_until" not in LAST_WHOAMI, LAST_WHOAMI[:240])
    one.sub, one.email, one.verified = saved

    first = connect(opener, base, "one")
    first_id = whoami_subject(opener, base, first) if first else ""
    status, _, raw = request(
        opener, "POST", base + "/auth/agent/start",
        urllib.parse.urlencode({"client_id": "devagotchi-cli", "agent": "codex"}).encode(),
        {"Content-Type": "application/x-www-form-urlencoded"},
    )
    extra = json.loads(raw) if status == 200 else {}
    claim_via(base, "one", extra.get("claim_code") or "")
    merged_id = whoami_subject(opener, base, extra.get("access_token") or "") if extra.get("access_token") else ""
    check("claim with an existing verified email merges", merged_id == first_id and merged_id.startswith("dg_"), f"{merged_id} {first_id}")
    opener2 = urllib.request.build_opener(urllib.request.HTTPCookieProcessor())
    again = connect(opener2, base, "one")
    again_id = whoami_subject(opener2, base, again) if again else ""
    check("returning subject is the same account", first_id.startswith("dg_") and first_id == again_id, f"{first_id} {again_id}")

    opener3 = urllib.request.build_opener(urllib.request.HTTPCookieProcessor())
    linked = connect(opener3, base, "two")
    linked_id = whoami_subject(opener3, base, linked) if linked else ""
    check("second provider with the same verified email links", first_id.startswith("dg_") and linked_id == first_id, f"{linked_id} {first_id}")

    opener4 = urllib.request.build_opener(urllib.request.HTTPCookieProcessor())
    fresh = connect(opener4, base, "plain")
    fresh_id = whoami_subject(opener4, base, fresh) if fresh else ""
    check("unverified email does not link", fresh_id and fresh_id != first_id, f"{fresh_id} {first_id}")
finally:
    hub.terminate()
    try:
        hub.wait(10)
    except subprocess.TimeoutExpired:
        hub.kill()

if failures:
    raise SystemExit(1)
print("oauth probe ok", flush=True)
