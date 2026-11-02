#!/usr/bin/env python3
"""Real AI clients signing in to a hub in `auth.mode hub`: the MCP authorization flow end to end.

Starts `devagotchi hub serve` in `auth.mode hub` on SQLite, claims it in Chromium with a
virtual passkey (as tests/claim_browser.py does), and puts a recording proxy in front of it
at the hub's `public_url`, so every request a client makes is captured (secrets redacted).
Then each client connects the way it does for a person, and the person's approval is the
hub's consent page in that browser:

1. protocol: this harness, step by step as the MCP authorization spec says: the 401 from
   /mcp, protected-resource metadata (RFC 9728), authorization-server metadata (RFC 8414),
   dynamic registration (RFC 7591), authorize with PKCE S256 and `resource` (RFC 8707),
   consent after a passkey sign-in, `iss` on the redirect (RFC 9207), token, initialize,
   tools/list, `whoami` and `machines`, refresh rotation, revocation (RFC 7009).
2. sdk: the official MCP TypeScript SDK: its own discovery, registration, PKCE, token,
   tools/list, `whoami`, and a refresh it does by itself on a 401.
3. codex: the real Codex CLI in a temporary CODEX_HOME with a fixed callback port and the
   file credential store: `codex mcp add --url` (which finds the hub wants OAuth and signs in
   at once), `codex mcp login` again, the token Codex stores, `codex mcp list`, and Codex's
   own app server listing the hub's tools and calling `whoami` with that token. No OpenAI
   key: nothing here reaches a model.

Both clients are pinned with integrity hashes by tests/clients/package-lock.json. CI runs
this on ubuntu-24.04 (`e2e`). `--receipt FILE` writes what ran, the versions and each
step's result, with every request the clients made. No token, code, verifier, state or
claim code is printed or written.

    npm ci --prefix tests/clients --ignore-scripts
    python3 tests/clients_oauth.py --devagotchi "$PWD/target/debug/devagotchi" \
        [--receipt verification/clients-oauth.json] [--channel chrome]

Needs Python Playwright with its Chromium (tests/requirements-browser.txt) and Node 20+.
"""

import argparse
import base64
import hashlib
import http.client
import json
import os
import platform
import re
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
PROTOCOL_VERSION = "2025-11-25"
# Values a receipt may hold. Everything else a client sends is replaced by "…".
SAFE = {
    "response_type", "code_challenge_method", "scope", "resource", "redirect_uri",
    "grant_type", "client_name", "token_endpoint_auth_method", "iss", "error",
    "error_description", "decision", "redirect_uris", "grant_types", "response_types",
    "application_type", "token_type", "expires_in", "software_id", "software_version", "prompt",
}
HOP = {"connection", "keep-alive", "transfer-encoding", "proxy-connection", "upgrade", "te"}


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def until(check, what: str, seconds: float = 20):
    deadline = time.time() + seconds
    while True:
        found = check()
        if found:
            return found
        if time.time() > deadline:
            raise SystemExit(f"timed out waiting until {what}")
        time.sleep(0.05)


def redact(params: dict) -> dict:
    out = {}
    for key, value in params.items():
        if key == "client_id":
            out[key] = str(value)[:4] + "…"
        elif key in SAFE:
            out[key] = value
        else:
            out[key] = "…"
    return out


def redact_url(url: str) -> str:
    parts = urllib.parse.urlsplit(url)
    query = redact(dict(urllib.parse.parse_qsl(parts.query)))
    return urllib.parse.urlunsplit(parts._replace(query=urllib.parse.urlencode(query, safe="…/:")))


class Recorder:
    """A reverse proxy at the hub's public URL that records what each client sends."""

    def __init__(self, upstream: int):
        self.upstream = upstream
        self.label = "setup"
        self.entries = []
        recorder = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def do_ANY(self):
                recorder.forward(self)

            do_GET = do_POST = do_DELETE = do_OPTIONS = do_PUT = do_ANY

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def forward(self, handler: BaseHTTPRequestHandler) -> None:
        length = int(handler.headers.get("Content-Length") or 0)
        body = handler.rfile.read(length) if length else b""
        headers = {k: v for k, v in handler.headers.items() if k.lower() not in HOP}
        upstream = http.client.HTTPConnection("127.0.0.1", self.upstream, timeout=120)
        try:
            upstream.request(handler.command, handler.path, body or None, headers)
            answer = upstream.getresponse()
        except OSError:
            handler.send_error(502)
            return
        sse = "text/event-stream" in (answer.getheader("Content-Type") or "")
        content = b"" if sse else answer.read()
        self.record(handler, body, answer, content)
        handler.send_response(answer.status, answer.reason)
        for key, value in answer.getheaders():
            if key.lower() not in HOP and key.lower() != "content-length":
                handler.send_header(key, value)
        if sse:
            handler.send_header("Connection", "close")
            handler.end_headers()
            try:
                while chunk := answer.read1(65536):
                    handler.wfile.write(chunk)
                    handler.wfile.flush()
            except (OSError, ValueError, http.client.HTTPException):
                pass  # either side ended the stream
            handler.close_connection = True
        else:
            handler.send_header("Content-Length", str(len(content)))
            handler.end_headers()
            handler.wfile.write(content)
        upstream.close()

    def record(self, handler, body: bytes, answer, content: bytes) -> None:
        agent = handler.headers.get("User-Agent", "")
        client = "browser" if "Chrome/" in agent else self.label
        path, _, query = handler.path.partition("?")
        entry = {"client": client, "method": handler.command, "path": path}
        if client != "browser" and agent:
            entry["user_agent"] = agent[:80]
        if query:
            entry["query"] = redact(dict(urllib.parse.parse_qsl(query)))
        if handler.headers.get("Authorization"):
            entry["authorization"] = handler.headers["Authorization"].split(" ")[0]
        kind = handler.headers.get("Content-Type", "")
        try:
            if body and "json" in kind:
                sent = json.loads(body)
                if path == "/mcp":
                    calls = sent if isinstance(sent, list) else [sent]
                    entry["mcp"] = [
                        c.get("method", "response")
                        + (f" {c['params']['name']}" if c.get("method") == "tools/call" else "")
                        for c in calls
                    ]
                else:
                    entry["body"] = redact(sent)
            elif body and "x-www-form-urlencoded" in kind:
                entry["body"] = redact(dict(urllib.parse.parse_qsl(body.decode())))
        except (ValueError, AttributeError, KeyError):
            entry["body"] = "unparsed"
        entry["status"] = answer.status
        for name in ("WWW-Authenticate", "Location"):
            if value := answer.getheader(name):
                entry[name.lower()] = redact_url(value) if name == "Location" else value
        if path.startswith("/.well-known/") and answer.status == 200:
            entry["response"] = json.loads(content)
        elif path in ("/oauth/register", "/oauth/token") and content:
            got = json.loads(content)
            entry["response"] = redact(got) if isinstance(got, dict) else "…"
        if client != "browser" or path.startswith("/oauth/"):
            self.entries.append(entry)


def fetch(method, url, body=None, headers=None, form=False):
    """(status, headers, body bytes) for one request; never raises on an HTTP status."""
    data = None
    headers = dict(headers or {})
    if body is not None:
        if form:
            data = urllib.parse.urlencode(body).encode()
            headers["Content-Type"] = "application/x-www-form-urlencoded"
        else:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as answer:
            return answer.status, answer.headers, answer.read()
    except urllib.error.HTTPError as refused:
        return refused.code, refused.headers, refused.read()
    except OSError:
        return 0, {}, b""


class Callback:
    """A loopback redirect target for one authorization answer."""

    def __init__(self):
        self.params = None
        callback = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                callback.params = dict(urllib.parse.parse_qsl(urllib.parse.urlsplit(self.path).query))
                self.send_response(200)
                self.send_header("Content-Type", "text/plain")
                self.end_headers()
                self.wfile.write(b"You can close this tab.")

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.uri = f"http://127.0.0.1:{self.server.server_address[1]}/callback"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def wait(self) -> dict:
        until(lambda: self.params is not None, "the redirect reached the client")
        self.server.shutdown()
        return self.params


class Steps:
    """Each client's steps, printed as they pass and kept for the receipt."""

    def __init__(self):
        self.clients = {}

    def ok(self, client: str, step: str, detail=None) -> None:
        done = {"step": step, "result": "pass"}
        if detail is not None:
            done["detail"] = detail
        self.clients.setdefault(client, []).append(done)
        print(f"PASS {client}: {step}")


def consent(page, url: str, authorize: str, sign_in: bool) -> str:
    """The person's side: (sign in with the passkey,) see the app, press Connect. Returns
    the app's name as the page showed it."""
    try:
        return approve(page, url, authorize, sign_in)
    except Exception as failed:
        raise SystemExit(f"consent failed at {redact_url(page.url)}: {failed}\n"
                         + page.inner_text("body")[:1000])


def approve(page, url: str, authorize: str, sign_in: bool) -> str:
    page.goto(authorize)
    if sign_in:
        page.wait_for_url(re.compile(re.escape(url) + r"/auth/login\?return_to=%2Foauth%2Fauthorize.*"))
        page.click("#passkey-sign-in")
        page.wait_for_url(re.compile(re.escape(url) + r"/oauth/authorize\?.*"), timeout=15_000)
    heading = page.inner_text("h1")
    assert heading.startswith("Connect "), page.inner_text("main")
    page.click("button[value=approve]")
    page.wait_for_url(lambda now: not now.startswith(url), timeout=15_000)
    return heading.removeprefix("Connect ").rstrip("?")


def mcp(url: str, token: str):
    """A minimal Streamable HTTP MCP session with a bearer token: initialize, then calls."""
    session = {}

    def call(method, params=None, notify=False):
        headers = {"Authorization": f"Bearer {token}", "Accept": "application/json, text/event-stream",
                   "MCP-Protocol-Version": PROTOCOL_VERSION}
        if "id" in session:
            headers["Mcp-Session-Id"] = session["id"]
        message = {"jsonrpc": "2.0", "method": method, "params": params or {}}
        if not notify:
            message["id"] = secrets.randbelow(1 << 30)
        status, got, body = fetch("POST", url + "/mcp", message, headers)
        if got.get("Mcp-Session-Id"):
            session["id"] = got["Mcp-Session-Id"]
        if notify:
            return status
        assert status == 200, (method, status, body[:200])
        if "text/event-stream" in got.get("Content-Type", ""):
            data = [line[5:] for line in body.decode().splitlines() if line.startswith("data:")]
            body = next(d for d in data if d.strip().startswith("{") and '"id"' in d)
        answer = json.loads(body)
        assert "result" in answer, answer.get("error")
        return answer["result"]

    init = call("initialize", {"protocolVersion": PROTOCOL_VERSION, "capabilities": {},
                               "clientInfo": {"name": "clients_oauth.py", "version": "1"}})
    call("notifications/initialized", notify=True)
    return init, call


def protocol(url: str, page, steps: Steps, rec: Recorder) -> None:
    rec.label = name = "protocol"
    status, headers, _ = fetch("POST", url + "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "initialize"},
                              {"Accept": "application/json, text/event-stream"})
    challenge = headers.get("WWW-Authenticate", "")
    found = re.search(r'resource_metadata="([^"]+)"', challenge)
    assert status == 401 and challenge.startswith("Bearer ") and found, (status, challenge)
    steps.ok(name, "401 from /mcp names the protected-resource metadata", challenge)

    status, _, body = fetch("GET", found.group(1))
    prm = json.loads(body)
    assert status == 200 and prm["authorization_servers"], prm
    resource = prm["resource"]
    assert (url + "/mcp").startswith(resource), f"{resource} does not cover {url}/mcp"
    status, _, body = fetch("GET", url + "/.well-known/oauth-protected-resource/mcp")
    assert status == 200 and json.loads(body)["resource"] == url + "/mcp"
    steps.ok(name, "protected-resource metadata (RFC 9728), root and /mcp forms", prm)

    issuer = prm["authorization_servers"][0]
    status, _, body = fetch("GET", issuer.rstrip("/") + "/.well-known/oauth-authorization-server")
    meta = json.loads(body)
    assert status == 200 and meta["issuer"] == issuer, meta
    assert "S256" in meta["code_challenge_methods_supported"], meta
    assert "none" in meta["token_endpoint_auth_methods_supported"], meta
    steps.ok(name, "authorization-server metadata (RFC 8414)", meta)

    callback = Callback()
    status, _, body = fetch("POST", meta["registration_endpoint"], {
        "client_name": "clients_oauth.py protocol walk", "redirect_uris": [callback.uri],
        "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"],
        "token_endpoint_auth_method": "none"})
    registered = json.loads(body)
    assert status == 201 and registered["client_id"], registered
    client_id = registered["client_id"]
    steps.ok(name, "dynamic registration (RFC 7591)", redact(registered))

    verifier = secrets.token_urlsafe(48)
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    sent_state = secrets.token_urlsafe(16)
    authorize = meta["authorization_endpoint"] + "?" + urllib.parse.urlencode({
        "response_type": "code", "client_id": client_id, "redirect_uri": callback.uri,
        "code_challenge": challenge, "code_challenge_method": "S256", "state": sent_state,
        "scope": " ".join(prm.get("scopes_supported", [])), "resource": url + "/mcp"})
    shown = consent(page, url, authorize, sign_in=True)
    answer = callback.wait()
    assert shown == "clients_oauth.py protocol walk", shown
    assert answer.get("state") == sent_state and answer.get("iss") == issuer and answer.get("code"), redact(answer)
    steps.ok(name, "authorize with PKCE and resource; passkey sign-in, then consent; state and iss back",
             redact(answer))

    status, _, body = fetch("POST", meta["token_endpoint"], {
        "grant_type": "authorization_code", "code": answer["code"], "redirect_uri": callback.uri,
        "client_id": client_id, "code_verifier": verifier, "resource": url + "/mcp"}, form=True)
    tokens = json.loads(body)
    assert status == 200 and tokens["token_type"] == "Bearer", redact(tokens)
    assert tokens["access_token"].startswith("dgh_") and tokens["refresh_token"].startswith("dgr_")
    steps.ok(name, "token for code + verifier + resource", redact(tokens))

    _, call = mcp(url, tokens["access_token"])
    tools = [tool["name"] for tool in call("tools/list")["tools"]]
    assert {"whoami", "machines"} <= set(tools), tools
    who = call("tools/call", {"name": "whoami", "arguments": {}})["structuredContent"]
    assert who["owner"] is True and who["via"] == "hub", who
    machines = call("tools/call", {"name": "machines", "arguments": {}})["structuredContent"]
    assert isinstance(machines["machines"], list), machines
    steps.ok(name, "initialize, tools/list, whoami and machines with the token",
             {"tools": len(tools), "whoami": {"owner": who["owner"], "via": who["via"]}})

    status, _, body = fetch("POST", meta["token_endpoint"], {
        "grant_type": "refresh_token", "refresh_token": tokens["refresh_token"],
        "client_id": client_id, "resource": url + "/mcp"}, form=True)
    rotated = json.loads(body)
    assert status == 200 and rotated["refresh_token"] != tokens["refresh_token"], redact(rotated)
    status, _, _ = fetch("POST", meta["token_endpoint"], {
        "grant_type": "refresh_token", "refresh_token": tokens["refresh_token"],
        "client_id": client_id}, form=True)
    assert status == 400
    status, _, _ = fetch("POST", url + "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "ping"},
                        {"Authorization": f"Bearer {rotated['access_token']}",
                         "Accept": "application/json, text/event-stream"})
    assert status == 401, "replaying a spent refresh token ends the family"
    steps.ok(name, "refresh rotates; the spent one replayed ends the family")

    status, _, _ = fetch("POST", meta["revocation_endpoint"], {"token": "dgr_unknown"}, form=True)
    assert status == 200
    steps.ok(name, "revocation (RFC 7009) answers 200 for an unknown token")


def sdk(url: str, page, steps: Steps, rec: Recorder, sdk_dir: Path, node: str) -> dict:
    rec.label = name = "sdk"
    version = json.loads((sdk_dir / "node_modules/@modelcontextprotocol/sdk/package.json").read_text())["version"]
    errors = Path(tempfile.mkstemp(prefix="sdk-client-", suffix=".err")[1])
    run = subprocess.Popen([node, str(sdk_dir / "sdk_client.mjs"), url + "/mcp", str(free_port())],
                           stdout=subprocess.PIPE, stderr=open(errors, "w"), text=True)
    try:
        first = json.loads(run.stdout.readline() or "{}")
        if "authorize" not in first:
            raise SystemExit("the SDK did not ask for authorization: " + errors.read_text()[-2000:])
        steps.ok(name, "SDK discovered the hub from the 401 and registered; asks for authorization",
                 redact_url(first["authorize"]))
        shown = consent(page, url, first["authorize"], sign_in=False)
        steps.ok(name, "consent page shows the SDK's registered name", shown)
        last = json.loads(run.stdout.readline() or "{}")
        run.wait(timeout=60)
        if not last.get("ok"):
            raise SystemExit("the SDK client failed: " + errors.read_text()[-2000:])
    finally:
        if run.poll() is None:
            run.kill()
    assert {"whoami", "machines"} <= set(last["tools"]), last
    assert last["whoami"]["owner"] is True and last["whoami"]["via"] == "hub", last
    steps.ok(name, "token, tools/list and whoami through the SDK", {k: v for k, v in last.items() if k != "tools"})
    assert last["refreshed"] and last["machines"], last
    steps.ok(name, "the SDK refreshed by itself on a 401 and called machines")
    return {"@modelcontextprotocol/sdk": version}


class AppServer:
    """Codex's app server over stdio (JSON-RPC, one message per line)."""

    def __init__(self, codex: str, env: dict):
        self.process = subprocess.Popen([codex, "app-server"], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                        text=True, env=env)
        self.next = 0

    def call(self, method: str, params: dict, seconds: float = 60):
        self.next += 1
        ident = self.next
        self.process.stdin.write(json.dumps({"id": ident, "method": method, "params": params}) + "\n")
        self.process.stdin.flush()
        deadline = time.time() + seconds
        while time.time() < deadline:
            line = self.process.stdout.readline()
            if not line:
                raise SystemExit(f"codex app-server exited during {method}")
            message = json.loads(line)
            if message.get("id") == ident and "method" not in message:
                if "error" in message:
                    raise SystemExit(f"codex app-server {method}: {message['error']}")
                return message["result"]
        raise SystemExit(f"codex app-server did not answer {method}")

    def notify(self, method: str) -> None:
        self.process.stdin.write(json.dumps({"method": method}) + "\n")
        self.process.stdin.flush()

    def close(self) -> None:
        self.process.kill()
        self.process.wait(timeout=10)


def codex(url: str, page, steps: Steps, rec: Recorder, codex_bin: str, work: Path) -> dict:
    rec.label = name = "codex"
    version = subprocess.run([codex_bin, "--version"], capture_output=True, text=True).stdout.strip()
    home = work / "codex-home"
    home.mkdir()
    port = free_port()
    (home / "config.toml").write_text(
        f'mcp_oauth_callback_port = {port}\nmcp_oauth_credentials_store = "file"\n')
    env = {k: v for k, v in os.environ.items() if not k.startswith(("OPENAI_", "CODEX_"))}
    env.update({"CODEX_HOME": str(home), "BROWSER": "true", "NO_COLOR": "1"})
    def authorize(argv: list, sign_in: bool) -> list:
        """Run a Codex command that signs in: the URL it prints, the consent, its exit."""
        run = subprocess.Popen([codex_bin, *argv], stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, text=True, env=env)
        printed = []
        threading.Thread(target=lambda: printed.extend(iter(run.stdout.readline, "")),
                         daemon=True).start()
        pattern = re.compile(re.escape(url) + r"/oauth/authorize\?\S+")
        try:
            found = until(lambda: run.poll() is not None or next(
                (m.group(0) for line in printed if (m := pattern.search(line))), None),
                f"codex {argv[1]} printed the authorization URL", 60)
            if found is True:
                raise SystemExit(f"codex {argv[1]} exited: " + "".join(printed)[-2000:])
            sent = dict(urllib.parse.parse_qsl(urllib.parse.urlsplit(found).query))
            steps.ok(name, f"codex mcp {argv[1]}: discovery and registration, then the "
                     "authorization URL", redact(sent))
            shown = consent(page, url, found, sign_in=sign_in)
            run.wait(timeout=60)
        finally:
            if run.poll() is None:
                run.kill()
        said = [line.strip() for line in printed if line.strip() and "?" not in line]
        assert run.returncode == 0, said
        steps.ok(name, f"codex mcp {argv[1]}: consent shows {shown!r}; exits 0", said[-1:])
        return said

    # `mcp add` finds the hub asks for OAuth and signs in at once.
    said = authorize(["mcp", "add", "devagotchi", "--url", url + "/mcp"], sign_in=False)
    assert "Detected OAuth support. Starting OAuth flow…" in said, said
    stored = home / ".credentials.json"
    assert stored.is_file() and "devagotchi" in stored.read_text(), "codex stored no credentials"
    steps.ok(name, "codex stored the token (mcp_oauth_credentials_store = file)")
    # `mcp login` again, as a person does when a connection has lapsed.
    authorize(["mcp", "login", "devagotchi"], sign_in=False)

    listed = subprocess.run([codex_bin, "mcp", "list", "--json"], capture_output=True, text=True,
                            env=env, timeout=60)
    entry = next(s for s in json.loads(listed.stdout) if s["name"] == "devagotchi")
    steps.ok(name, "codex mcp list --json", {k: v for k, v in entry.items() if k in ("name", "auth_status", "enabled")})

    app = AppServer(codex_bin, env)
    try:
        app.call("initialize", {"clientInfo": {"name": "clients_oauth", "version": "1"}})
        app.notify("initialized")
        status = app.call("mcpServerStatus/list", {"serverName": "devagotchi"})
        server = next(s for s in status["data"] if s["name"] == "devagotchi")
        tools = sorted(server.get("tools") or {})
        assert {"whoami", "machines"} <= set(tools), server
        steps.ok(name, "codex app-server: mcpServerStatus/list lists the hub's tools with the stored token",
                 {"tools": len(tools), "auth_status": server.get("authStatus")})
        # A thread holds the MCP connection a tool call goes through; starting one asks no model.
        thread_id = app.call("thread/start", {})["thread"]["id"]
        called = app.call("mcpServer/tool/call", {"server": "devagotchi", "threadId": thread_id,
                                                   "tool": "whoami", "arguments": {}})
        who = called.get("structuredContent") or json.loads(called["content"][0]["text"])
        assert who["owner"] is True and who["via"] == "hub", who
        steps.ok(name, "codex app-server: thread/start, then mcpServer/tool/call whoami",
                 {"owner": who["owner"], "via": who["via"]})
    finally:
        app.close()
    return {"codex": version}


def claim(page, url: str, log: Path) -> None:
    """Claim the new hub as its owner, with the code it printed (never shown here)."""
    link = until(lambda: re.findall(rf"{re.escape(url)}/claim#[0-9A-Z-]+", log.read_text()),
                 "the hub printed its claim link")[0]
    page.goto(link)
    until(lambda: not page.evaluate("location.hash"), "the claim code left the address bar")
    page.fill("#claim-name", "Owner")
    page.click("#passkey-claim")
    page.wait_for_url(f"{url}/account", timeout=15_000)
    assert "You own this hub" in page.inner_text("main")
    page.click("header form button")
    page.wait_for_url(f"{url}/signed-out")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--devagotchi", required=True, help="absolute path of the binary")
    parser.add_argument("--codex", default=str(HERE / "clients/node_modules/.bin/codex"),
                        help="the Codex CLI (tests/clients after npm ci); 'skip' to leave it out")
    parser.add_argument("--sdk-dir", default=str(HERE / "clients"), help="tests/clients after npm ci")
    parser.add_argument("--node", default="node")
    parser.add_argument("--channel", default=None, help="an installed browser (e.g. chrome)")
    parser.add_argument("--receipt", help="write the run's receipt (JSON) here")
    args = parser.parse_args()
    from playwright.sync_api import sync_playwright

    started = time.time()
    work = Path(tempfile.mkdtemp(prefix="clients-oauth-"))
    hub_port = free_port()
    rec = Recorder(hub_port)
    url = f"http://localhost:{rec.port}"
    config = work / "hub.json"
    config.write_text(json.dumps({
        "listen": f"127.0.0.1:{hub_port}", "public_url": url, "auth": {"mode": "hub"},
        "storage": {"url": f"sqlite:{work / 'accounts.sqlite3'}"}, "brokers": [], "grants": []}))
    env = {k: v for k, v in os.environ.items() if not k.startswith("DEVAGOTCHI_HUB_STORAGE")}
    log = work / "hub.log"
    hub = subprocess.Popen([args.devagotchi, "hub", "serve", "--state-dir", str(work / "state"),
                            "--config", str(config)], stdout=subprocess.DEVNULL,
                           stderr=open(log, "w"), env=env)
    steps = Steps()
    versions = {"python": platform.python_version()}
    try:
        until(lambda: hub.poll() is not None or fetch("GET", url + "/healthz")[0] == 200,
              "the hub answered /healthz", 30)
        assert hub.poll() is None, log.read_text()
        with sync_playwright() as p:
            browser = p.chromium.launch(channel=args.channel)
            versions["browser"] = f"{browser.browser_type.name} {browser.version}"
            page = browser.new_context().new_page()
            cdp = page.context.new_cdp_session(page)
            cdp.send("WebAuthn.enable")
            cdp.send("WebAuthn.addVirtualAuthenticator", {"options": {
                "protocol": "ctap2", "transport": "internal", "hasResidentKey": True,
                "hasUserVerification": True, "isUserVerified": True,
                "automaticPresenceSimulation": True}})
            claim(page, url, log)
            steps.ok("hub", "claimed in the browser with a virtual passkey; signed out")
            protocol(url, page, steps, rec)
            node = subprocess.run([args.node, "--version"], capture_output=True, text=True).stdout.strip()
            versions["node"] = node
            versions.update(sdk(url, page, steps, rec, Path(args.sdk_dir), args.node))
            if args.codex != "skip":
                versions.update(codex(url, page, steps, rec, args.codex, work))
            else:
                print("SKIP codex: --codex skip")
            browser.close()
    finally:
        hub.terminate()
        hub.wait(timeout=10)
    seconds = round(time.time() - started, 1)
    if args.receipt:
        commit = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True,
                                cwd=HERE).stdout.strip()
        receipt = {
            "status": "passed",
            "what": "tests/clients_oauth.py: real MCP clients through the hub's own OAuth 2.1 "
                    "server (auth.mode hub, SQLite) on loopback, consent with a virtual passkey",
            "recorded_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "seconds": seconds,
            "source_commit": commit,
            "platform": f"{platform.system()} {platform.machine()}",
            "versions": versions,
            "steps": steps.clients,
            "requests": rec.entries,
            "redaction": "codes, verifiers, challenges, state, tokens and claim codes are '…'; "
                         "client ids keep their 4-character prefix",
        }
        Path(args.receipt).write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"CLIENTS OAUTH PASSED in {seconds} s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
