#!/usr/bin/env python3
"""`devagotchi login --hub` against a hub with its own accounts, approved in a real browser.

Starts `devagotchi hub serve` in `auth.mode hub` on SQLite at http://localhost (a secure
context, so WebAuthn works without TLS), makes the owner's passkey in Chromium with the
Chrome DevTools virtual authenticator, then runs the CLI as a person would, in a temporary
config directory:

1. `devagotchi login --hub URL` prints the hub's `/device` link and a code (RFC 8628, the
   endpoints from the hub's RFC 8414 metadata). Signed out, the link sends the browser to
   the passkey sign-in and back; the page shows the same code; Approve signs the terminal
   in, and `login` exits 0 naming the account.
2. `whoami --json`: the account at that hub, its id and name, from an owner-only
   `account.json`.
3. `devagotchi mcp --hub` relays an MCP session with the stored token: `whoami` on the hub
   says the same account, as an AI connection (`via: hub`).
4. A token due for refresh is refreshed at the hub's token endpoint, and rotates.
5. `logout` revokes at the hub: the old token is refused, and the file is gone.
6. Cancel on the page: `login` exits non-zero and says so; nothing is written.

No token or code is printed. Needs Python Playwright with its Chromium
(tests/requirements-browser.txt). CI runs it on ubuntu-24.04 (`e2e`); locally:

    python3 tests/device_login_browser.py --devagotchi "$PWD/target/debug/devagotchi"
"""

import argparse
import hashlib
import json
import os
import re
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path


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


def get(url: str, token: str = None) -> int:
    request = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"} if token else {})
    try:
        with urllib.request.urlopen(request, timeout=10) as answer:
            return answer.status
    except urllib.error.HTTPError as refused:
        return refused.code
    except OSError:
        return 0


def digest(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


class Login:
    """`devagotchi login --hub URL` running, with what it has said so far."""

    def __init__(self, devagotchi: str, hub: str, env: dict):
        self.process = subprocess.Popen([devagotchi, "login", "--hub", hub], env=env,
                                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        self.said = []
        threading.Thread(target=lambda: self.said.extend(iter(self.process.stdout.readline, "")),
                         daemon=True).start()

    def link(self, hub: str) -> tuple:
        """The `/device?code=` link and the code it printed."""
        pattern = re.compile(re.escape(hub) + r"/device\?code=([B-Z]{4}-[B-Z]{4})")
        found = until(lambda: self.process.poll() is not None or next(
            (m for line in self.said if (m := pattern.search(line))), None),
            "login printed its link", 30)
        if found is True:
            raise SystemExit("login exited: " + "".join(self.said))
        return found.group(0), found.group(1)

    def finish(self) -> int:
        self.process.wait(timeout=60)
        time.sleep(0.1)
        return self.process.returncode


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--devagotchi", required=True, help="absolute path of the binary")
    parser.add_argument("--channel", default=None, help="an installed browser (e.g. chrome)")
    args = parser.parse_args()
    from playwright.sync_api import sync_playwright

    work = Path(tempfile.mkdtemp(prefix="device-login-"))
    port = free_port()
    url = f"http://localhost:{port}"
    config = work / "hub.json"
    config.write_text(json.dumps({
        "listen": f"127.0.0.1:{port}", "public_url": url, "auth": {"mode": "hub"},
        "storage": {"url": f"sqlite:{work / 'accounts.sqlite3'}"}, "brokers": [], "grants": []}))
    env = {k: v for k, v in os.environ.items() if not k.startswith("DEVAGOTCHI_")}
    hub = subprocess.Popen([args.devagotchi, "hub", "serve", "--state-dir", str(work / "state"),
                            "--config", str(config)], stdout=subprocess.DEVNULL,
                           stderr=open(work / "hub.log", "w"), env=env)
    cli_env = {**env, "DEVAGOTCHI_CONFIG_DIR": str(work / "config")}
    account_file = work / "config" / "account.json"
    run = lambda *argv: subprocess.run([args.devagotchi, *argv], env=cli_env, capture_output=True,  # noqa: E731
                                       text=True, timeout=60)
    try:
        until(lambda: hub.poll() is not None or get(url + "/healthz") == 200, "the hub answered", 30)
        assert hub.poll() is None, (work / "hub.log").read_text()
        owner_link = subprocess.run(
            [args.devagotchi, "hub", "owner", "--state-dir", str(work / "state"), "--config",
             str(config), "--name", "Jesse"], capture_output=True, text=True, check=True, env=env,
        ).stdout.strip()

        with sync_playwright() as p:
            browser = p.chromium.launch(channel=args.channel)
            page = browser.new_context().new_page()
            cdp = page.context.new_cdp_session(page)
            cdp.send("WebAuthn.enable")
            cdp.send("WebAuthn.addVirtualAuthenticator", {"options": {
                "protocol": "ctap2", "transport": "internal", "hasResidentKey": True,
                "hasUserVerification": True, "isUserVerified": True,
                "automaticPresenceSimulation": True}})
            page.goto(owner_link)
            page.click("#passkey-register")
            page.wait_for_url(f"{url}/account", timeout=15_000)
            page.click("header form button")
            page.wait_for_url(f"{url}/signed-out")
            print("PASS the owner's passkey is made; signed out")

            # 1. login: the link, the passkey sign-in, the same code, Approve.
            login = Login(args.devagotchi, url, cli_env)
            link, code = login.link(url)
            page.goto(link)
            page.wait_for_url(re.compile(re.escape(url) + r"/auth/login\?return_to=.*"))
            page.click("#passkey-sign-in")
            page.wait_for_url(re.compile(re.escape(url) + r"/device\?code=.*"), timeout=15_000)
            text = page.inner_text("main")
            assert "Sign in Devagotchi CLI?" in text and code in text and "Jesse" in text, text
            page.click("button[value=approve]")
            page.wait_for_url(f"{url}/device", timeout=15_000)
            assert "Your terminal is signed in" in page.inner_text("main")
            assert login.finish() == 0, "".join(login.said)
            said = "".join(login.said)
            assert "signed in as Jesse at " + url in said, said
            print("PASS login: the hub's /device link, passkey sign-in, the same code, Approve")

            # 2. whoami, and the file only its owner reads.
            who = json.loads(run("whoami", "--json").stdout)
            assert who["signed_in"] and who["issuer"] == url and who["username"] == "Jesse", who
            assert who["subject"].startswith("acct_") and who["standing"] == "fresh", who
            if os.name == "posix":
                assert stat.S_IMODE(account_file.stat().st_mode) == 0o600
            stored = json.loads(account_file.read_text())["access_token"]
            assert stored.startswith("dgh_")
            print("PASS whoami: the hub's account, from an owner-only account.json")

            # 3. The relay: an MCP session as the account on file.
            relay = subprocess.run(
                [args.devagotchi, "mcp", "--hub", f"http://127.0.0.1:{port}"], env=cli_env,
                input="\n".join(json.dumps(m) for m in [
                    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                        "protocolVersion": "2025-06-18", "capabilities": {},
                        "clientInfo": {"name": "device_login_browser", "version": "1"}}},
                    {"jsonrpc": "2.0", "method": "notifications/initialized"},
                    {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                     "params": {"name": "whoami", "arguments": {}}},
                ]) + "\n", capture_output=True, text=True, timeout=60)
            answers = [json.loads(line) for line in relay.stdout.splitlines() if line.strip()]
            called = next(a for a in answers if a.get("id") == 2)
            seen = called["result"]["structuredContent"]
            assert seen["subject"] == who["subject"] and seen["via"] == "hub" and seen["owner"], seen
            print("PASS devagotchi mcp --hub relays as the account (whoami on the hub, via hub)")

            # 4. A token due for refresh is refreshed at the hub, and rotates.
            held = json.loads(account_file.read_text())
            held["expires_at"] = int(time.time()) + 60
            account_file.write_text(json.dumps(held))
            who = json.loads(run("whoami", "--json").stdout)
            now = json.loads(account_file.read_text())
            assert who["standing"] == "fresh", who
            assert digest(now["access_token"]) != digest(stored), "the access token rotated"
            assert digest(now["refresh_token"]) != digest(held["refresh_token"]), "the refresh token rotated"
            assert get(url + "/api/v1/me", stored) == 401, "the old access token ended with the rotation"
            assert get(url + "/api/v1/me", now["access_token"]) == 200
            print("PASS a token due for refresh is refreshed at the hub's token endpoint")

            # 5. logout revokes at the hub.
            out = run("logout")
            assert out.returncode == 0 and "tokens revoked at " + url in out.stderr, out.stderr
            assert not account_file.exists()
            assert get(url + "/api/v1/me", now["access_token"]) == 401, "revoked"
            print("PASS logout revokes at the hub and removes account.json")

            # 6. Cancel on the page: login says so, and writes nothing.
            login = Login(args.devagotchi, url, cli_env)
            link, code = login.link(url)
            page.goto(link)
            assert code in page.inner_text("main")
            page.click("button[value=deny]")
            page.wait_for_url(f"{url}/device", timeout=15_000)
            assert "Cancelled" in page.inner_text("main")
            assert login.finish() != 0
            assert "you cancelled the sign-in" in "".join(login.said), "".join(login.said)
            assert not account_file.exists()
            print("PASS Cancel on the page: login stops, nothing written")
            browser.close()
    finally:
        hub.terminate()
        hub.wait(timeout=10)
    print("DEVICE LOGIN BROWSER PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
