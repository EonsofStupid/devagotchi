#!/usr/bin/env python3
"""The operator page (`/operator`) in a real browser: an operator lets a waiting person in.

Starts `devagotchi hub serve` as a hosted hub (`tenancy.mode multi`) on SQLite at
http://localhost (a secure context, so WebAuthn works without TLS), with a site whose
`early_access.mode` is `early_access` and two plans. Then:

1. The operator signs in: `devagotchi hub owner` prints the owner's link, and Chromium makes
   the owner's passkey with the Chrome DevTools virtual authenticator.
2. A person's AI client connects as one does (dynamic registration, the consent page's
   Connect, PKCE): their account and tenant are made, and they wait. Their MCP call
   answers with the early-access message instead of acting; `/operator` is a 404 for them.
3. In a browser with JavaScript off (the page needs none), on a screen narrower than its
   tables, the operator's page shows the mode and the person waiting. The operator checks
   them, picks the Pro plan on their row and presses the one button.
4. The person's next MCP call works, on the Pro plan.
5. The operator changes their plan from their row; `whoami` says Free.

Needs Python Playwright with its Chromium (tests/requirements-browser.txt). CI runs it on
ubuntu-24.04 (`e2e`); locally:

    python3 tests/operator_browser.py --devagotchi "$PWD/target/debug/devagotchi"
"""

import argparse
import base64
import hashlib
import http.cookies
import json
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REDIRECT = "http://127.0.0.1:9/callback"


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_ready(url: str, process: subprocess.Popen) -> None:
    deadline = time.time() + 30
    while time.time() < deadline:
        if process.poll() is not None:
            raise SystemExit(f"the hub exited with {process.returncode}")
        try:
            with urllib.request.urlopen(url + "/healthz", timeout=1) as answer:
                if answer.status == 200:
                    return
        except OSError:
            time.sleep(0.2)
    raise SystemExit("the hub did not answer /healthz within 30 seconds")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


def call(url: str, data=None, headers=None, method=None):
    """One HTTP request, redirects not followed: status, headers, body."""
    request = urllib.request.Request(url, data=data, headers=headers or {}, method=method)
    try:
        with OPENER.open(request, timeout=15) as answer:
            return answer.status, answer.headers, answer.read().decode()
    except urllib.error.HTTPError as refused:
        return refused.code, refused.headers, refused.read().decode()


class Person:
    """An AI client connecting to the hub as a new person, with nothing but Connect."""

    def __init__(self, hub: str):
        self.hub = hub
        status, _, body = call(
            f"{hub}/oauth/register",
            json.dumps({"client_name": "Claude", "redirect_uris": [REDIRECT]}).encode(),
            {"Content-Type": "application/json"},
        )
        assert status in (200, 201), body
        client = json.loads(body)["client_id"]
        verifier = secrets.token_urlsafe(48)
        challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
        resource = f"{hub}/mcp"
        query = urllib.parse.urlencode({
            "response_type": "code", "client_id": client, "redirect_uri": REDIRECT,
            "code_challenge": challenge, "code_challenge_method": "S256", "state": "s1",
            "resource": resource,
        })
        status, headers, page = call(f"{hub}/oauth/authorize?{query}")
        jar = http.cookies.SimpleCookie()
        if status == 303 and headers["Location"].startswith("/auth/login?"):
            # A hosted hub without `anonymous_connect`: sign up with an email and a password,
            # then the same request goes straight back with the code (spec first-connect).
            back = urllib.parse.parse_qs(urllib.parse.urlsplit(headers["Location"]).query)["return_to"][0]
            status, headers, _ = call(f"{hub}/auth/password/new", urllib.parse.urlencode({
                "email": f"{secrets.token_hex(6)}@example.com", "password": secrets.token_urlsafe(12),
                "return_to": back,
            }).encode(), {"Origin": hub, "Content-Type": "application/x-www-form-urlencoded"})
            assert status == 303, status
            for value in headers.get_all("Set-Cookie") or []:
                jar.load(value)
            cookies = "; ".join(f"{name}={jar[name].value}" for name in ("dg_session", "dvg_fresh"))
            status, headers, _ = call(hub + back, headers={"Cookie": cookies})
        else:
            assert status == 200 and "Connect Claude?" in page, page
            status, headers, _ = call(
                f"{hub}/oauth/authorize",
                urllib.parse.urlencode({
                    "client_id": client, "redirect_uri": REDIRECT, "code_challenge": challenge,
                    "resource": resource, "scope": "mcp offline_access", "state": "s1",
                    "decision": "approve",
                }).encode(),
                {"Origin": hub, "Content-Type": "application/x-www-form-urlencoded"},
            )
        assert status == 303, status
        for value in headers.get_all("Set-Cookie") or []:
            jar.load(value)
        self.cookie = f"dg_session={jar['dg_session'].value}"
        location = headers["Location"]
        code = urllib.parse.parse_qs(urllib.parse.urlsplit(location).query)["code"][0]
        status, _, body = call(
            f"{hub}/oauth/token",
            urllib.parse.urlencode({
                "grant_type": "authorization_code", "code": code, "redirect_uri": REDIRECT,
                "client_id": client, "code_verifier": verifier,
            }).encode(),
            {"Content-Type": "application/x-www-form-urlencoded"},
        )
        assert status == 200, body
        self.token = json.loads(body)["access_token"]
        self.session = None

    def mcp(self, method: str, params: dict) -> dict:
        headers = {
            "Authorization": f"Bearer {self.token}",
            "Accept": "application/json, text/event-stream",
            "Content-Type": "application/json",
        }
        if self.session is None:
            status, answer, body = call(f"{self.hub}/mcp", json.dumps({
                "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                    "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "operator_browser", "version": "1"}}}).encode(), headers)
            assert status == 200, body
            self.session = answer["mcp-session-id"]
            call(f"{self.hub}/mcp", json.dumps(
                {"jsonrpc": "2.0", "method": "notifications/initialized"}).encode(),
                {**headers, "mcp-session-id": self.session})
        status, _, body = call(f"{self.hub}/mcp", json.dumps({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
            {**headers, "mcp-session-id": self.session})
        assert status == 200, body
        for line in body.splitlines():
            if line.startswith("data:") and line[5:].strip().startswith("{"):
                body = line[5:].strip()
                break
        return json.loads(body)["result"]

    def tool(self, name: str, arguments: dict = None) -> dict:
        return self.mcp("tools/call", {"name": name, "arguments": arguments or {}})


def site(work: Path) -> Path:
    """The built-in site with early access on and two plans."""
    value = json.loads((ROOT / "crates/hub/site/default/site.json").read_text())
    value["early_access"] = {"mode": "early_access"}
    value["plans"] = [
        {"id": "free", "name": "Free", "price": "Free", "default": True,
         "limits": {"projects": 1, "machines": 2}},
        {"id": "pro", "name": "Pro", "price": "Ask",
         "limits": {"projects": "unlimited", "machines": 5}},
    ]
    directory = work / "site"
    directory.mkdir()
    (directory / "site.json").write_text(json.dumps(value))
    return directory


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--devagotchi", required=True, help="absolute path of the binary")
    parser.add_argument("--allow-skip", action="store_true",
                        help="exit 0 when Playwright is missing instead of failing")
    parser.add_argument("--channel", default=None,
                        help="a browser already installed (e.g. chrome) instead of Playwright's")
    args = parser.parse_args()
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        print("NOT RUN: Python Playwright is not installed")
        return 0 if args.allow_skip else 2

    work = Path(tempfile.mkdtemp(prefix="operator-browser-"))
    state, config = work / "state", work / "hub.json"
    port = free_port()
    url = f"http://localhost:{port}"
    config.write_text(json.dumps({
        "listen": f"127.0.0.1:{port}",
        "public_url": url,
        "auth": {"mode": "hub"},
        "tenancy": {"mode": "multi", "starts_per_hour": 100, "anonymous_connect": True},
        "site": {"dir": str(site(work))},
        "brokers": [],
        "grants": [],
    }))
    hub = subprocess.Popen(
        [args.devagotchi, "hub", "serve", "--state-dir", str(state), "--config", str(config)],
        stdout=subprocess.DEVNULL,
        stderr=open(work / "hub.log", "w"),
    )
    try:
        wait_ready(url, hub)
        link = subprocess.run(
            [args.devagotchi, "hub", "owner", "--state-dir", str(state), "--config", str(config),
             "--name", "Jesse"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        assert link.startswith(f"{url}/owner#dgl_"), link

        with sync_playwright() as p:
            browser = p.chromium.launch(channel=args.channel)
            # 1. The operator signs in with a passkey.
            signing = browser.new_context()
            page = signing.new_page()
            cdp = page.context.new_cdp_session(page)
            cdp.send("WebAuthn.enable")
            cdp.send("WebAuthn.addVirtualAuthenticator", {"options": {
                "protocol": "ctap2", "transport": "internal", "hasResidentKey": True,
                "hasUserVerification": True, "isUserVerified": True,
                "automaticPresenceSimulation": True,
            }})
            page.goto(link)
            page.click("#passkey-register")
            page.wait_for_url(f"{url}/account", timeout=15_000)
            assert "You own this hub" in page.inner_text("main")
            assert page.locator("a[href='/operator']").count() == 1, "the account page leads there"
            print("PASS the operator signed in with a passkey")

            # 2. A person connects an AI client, and waits.
            person = Person(url)
            who = person.tool("whoami")["structuredContent"]
            assert who["access"] == "waiting", who
            tenant = who["tenant"]
            answer = person.tool("machines")
            assert answer.get("isError") is True and "structuredContent" not in answer, answer
            note = answer["content"][0]["text"]
            assert "early-access list" in note and f"{url}/account" in note, note
            status, _, missing = call(f"{url}/no-such-page")
            assert (status, missing) == (404, ""), (status, missing)
            assert call(f"{url}/operator", headers={"Cookie": person.cookie})[0] == 404
            assert call(f"{url}/operator")[0] == 404
            print("PASS the person waits, and the operator page is a 404 to them")

            # 3. The operator's page, with JavaScript off, on a screen narrower than its
            # tables (they scroll in their box): let them in on the Pro plan.
            plain = browser.new_context(java_script_enabled=False,
                                        viewport={"width": 390, "height": 800})
            plain.add_cookies(signing.cookies())
            page = plain.new_page()
            page.goto(f"{url}/operator")
            text = page.inner_text("main")
            assert "Sign-up is early access" in text and "hub-up.sh --site DIR --site-only" in text, text
            assert "Waiting to be let in (1)" in text and tenant in text, text
            page.get_by_role("checkbox", name=f"Let in {tenant}").check()
            page.get_by_role("combobox", name=f"Plan for {tenant}").select_option("pro")
            page.get_by_role("button", name="Let in the people checked").click()
            page.wait_for_url(f"{url}/operator?done=admitted&n=1#waiting")
            text = page.inner_text("main")
            assert "Let in: 1." in text and "Nobody is waiting." in text and "Let in (1)" in text, text
            print("PASS the operator let them in with one press, no script")

            # 4. Their next call works, on the Pro plan.
            answer = person.tool("machines")
            assert not answer.get("isError"), answer
            assert answer["structuredContent"]["machines"] == [], answer
            who = person.tool("whoami")["structuredContent"]
            assert (who["access"], who["plan"]) == ("admitted", "pro"), who
            print("PASS their next MCP call works, on the Pro plan")

            # 5. A plan change from their row.
            row = page.locator("#admitted tr", has_text=tenant)
            row.get_by_role("combobox", name=f"Plan for {tenant}").select_option("free")
            row.get_by_role("button", name="Change plan").click()
            page.wait_for_url(f"{url}/operator?done=plan#admitted")
            assert "Plan changed." in page.inner_text("main")
            assert person.tool("whoami")["structuredContent"]["plan"] == "free"
            print("PASS the operator changed their plan from their row")
            browser.close()
    finally:
        hub.terminate()
        hub.wait(timeout=10)
    print("OPERATOR BROWSER PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
