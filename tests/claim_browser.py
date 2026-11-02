#!/usr/bin/env python3
"""Claiming a new hub in a real browser: Chromium with a virtual passkey (16c).

Starts `devagotchi hub serve` in `auth.mode hub` on http://localhost (a secure context, so
WebAuthn works without TLS) with an empty store, reads the claim link the hub printed to its
log, and drives Chromium through it with the Chrome DevTools virtual authenticator. It checks
what the Rust tests cannot: that the claim page, the hub's script (`/assets/passkey.js`) and
the browser's WebAuthn API agree.

1. The landing page of a hub nobody owns offers the claim, and no sign-in.
2. The claim link opened in a tab already on /claim (only the hash changes) fills the code
   in, and the code leaves the address bar.
3. A cancelled passkey prompt is retried with the same code.
4. Claiming names the owner, makes their passkey and signs them in.
5. Signed out, the landing page offers sign-in and this hub's MCP address; the passkey
   signs back in.
6. The claimed hub refuses the code, and a restart prints no new one.

Needs Python Playwright with its Chromium (tests/requirements-browser.txt). CI runs it on
ubuntu-24.04 (`e2e`); locally:

    python3 tests/claim_browser.py --devagotchi "$PWD/target/debug/devagotchi"
"""

import argparse
import json
import re
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path


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


def serve(devagotchi: str, state: Path, config: Path, log: Path) -> subprocess.Popen:
    return subprocess.Popen(
        [devagotchi, "hub", "serve", "--state-dir", str(state), "--config", str(config)],
        stdout=subprocess.DEVNULL,
        stderr=open(log, "w"),
    )


def stop(hub: subprocess.Popen) -> None:
    hub.terminate()
    hub.wait(timeout=10)


def settled(page) -> str:
    """The status line once the page has finished a try. Polled from here: the pages'
    Content-Security-Policy refuses the eval that Playwright's own polling needs."""
    deadline = time.time() + 15
    while time.time() < deadline:
        text = page.inner_text("#passkey-status")
        if text and not text.startswith(("Making", "Waiting")):
            return text
        time.sleep(0.1)
    raise SystemExit("the claim page never finished: " + page.inner_text("#passkey-status"))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--devagotchi", required=True, help="absolute path of the binary")
    parser.add_argument("--allow-skip", action="store_true",
                        help="exit 0 when Playwright is missing instead of failing")
    args = parser.parse_args()
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        print("NOT RUN: Python Playwright is not installed")
        return 0 if args.allow_skip else 2

    work = Path(tempfile.mkdtemp(prefix="claim-browser-"))
    state, config = work / "state", work / "hub.json"
    port = free_port()
    url = f"http://localhost:{port}"
    config.write_text(json.dumps({
        "listen": f"127.0.0.1:{port}",
        "public_url": url,
        "auth": {"mode": "hub"},
        "brokers": [],
        "grants": [],
    }))
    hub = serve(args.devagotchi, state, config, work / "hub.log")
    try:
        wait_ready(url, hub)
        printed = re.findall(rf"{re.escape(url)}/claim#[0-9A-Z-]+", (work / "hub.log").read_text())
        assert len(printed) == 1, "the hub prints one claim link when it starts"
        link = printed[0]

        with sync_playwright() as p:
            browser = p.chromium.launch()
            page = browser.new_context().new_page()
            cdp = page.context.new_cdp_session(page)
            cdp.send("WebAuthn.enable")
            phone = cdp.send("WebAuthn.addVirtualAuthenticator", {"options": {
                "protocol": "ctap2", "transport": "internal", "hasResidentKey": True,
                "hasUserVerification": True, "isUserVerified": True,
                "automaticPresenceSimulation": True,
            }})["authenticatorId"]

            # 1. The landing page offers the claim, and nothing to sign in to.
            page.goto(url + "/")
            text = page.inner_text("body")
            assert "Nobody owns this hub yet" in text, text
            assert page.locator("a[href^='/auth/login']").count() == 0, text
            page.click("text=Claim this hub")
            page.wait_for_url(f"{url}/claim")
            assert page.input_value("#claim-code") == ""
            print("PASS the landing page of a new hub offers the claim")

            # 2. The link from the log, in the same tab: only the hash changes.
            page.goto(link)
            deadline = time.time() + 5
            while page.evaluate("location.hash") and time.time() < deadline:
                time.sleep(0.05)
            assert page.url == f"{url}/claim", f"the code stayed in the address bar: {page.url}"
            assert len(page.input_value("#claim-code")) == 29
            print("PASS the claim link fills the code in and leaves the address bar")

            # 3. The person cancels the browser's prompt: nothing is made; press again.
            cdp.send("WebAuthn.setUserVerified", {"authenticatorId": phone, "isUserVerified": False})
            page.fill("#claim-name", "Jesse")
            page.click("#passkey-claim")
            status = settled(page)
            assert "Try again" in status or "Cancelled" in status, status
            cdp.send("WebAuthn.setUserVerified", {"authenticatorId": phone, "isUserVerified": True})
            print("PASS a refused passkey prompt leaves the claim to retry")

            # 4. Claim: the owner named, the passkey made, signed in.
            page.click("#passkey-claim")
            page.wait_for_url(f"{url}/account", timeout=15_000)
            text = page.inner_text("main")
            assert "Signed in as Jesse" in text and "You own this hub" in text, text
            print("PASS the claim made the owner's passkey and signed them in")

            # 5. Signed out: sign-in and this hub's MCP address; the passkey signs back in.
            page.click("header form button")
            page.wait_for_url(f"{url}/signed-out")
            page.goto(url + "/")
            text = page.inner_text("main")
            assert f"{url}/mcp" in text and "Nobody owns" not in text, text
            page.goto(f"{url}/auth/login?return_to=/account")
            page.click("#passkey-sign-in")
            page.wait_for_url(f"{url}/account", timeout=15_000)
            print("PASS signed out, the landing page says where AIs connect; back in with the passkey")

            # 6. The code is spent for good.
            page.goto(link)
            assert "This hub has an owner" in page.inner_text("main")
            print("PASS the claimed hub refuses its code")
            browser.close()
    finally:
        stop(hub)

    hub = serve(args.devagotchi, state, config, work / "restart.log")
    try:
        wait_ready(url, hub)
        assert "/claim#" not in (work / "restart.log").read_text(), "a claimed hub prints no code"
        print("PASS a claimed hub prints no claim code when it restarts")
    finally:
        stop(hub)
    print("CLAIM BROWSER PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
