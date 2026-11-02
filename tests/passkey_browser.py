#!/usr/bin/env python3
"""The hub's own sign-in in a real browser: Chromium with a virtual passkey.

Starts `devagotchi hub serve` in `auth.mode hub` on http://localhost (a secure context, so
WebAuthn works without TLS), prints the owner's link with `devagotchi hub owner`, and drives
Chromium through it with the Chrome DevTools virtual authenticator (a platform passkey with
user verification). It checks what the Rust tests cannot: that the hub's own script
(`/assets/passkey.js`) and the browser's WebAuthn API agree.

1. The owner's link (`/owner#…`) makes a passkey and signs in; the token leaves the address bar.
2. The account page says who is signed in and that they own the hub.
3. Signing out ends the session; the sign-in page signs back in with the passkey alone.
4. The spent link no longer works.
5. `hub owner --replace` works on the device that already holds the owner's passkey.

Needs Python Playwright with its Chromium (tests/requirements-browser.txt). CI runs it on
ubuntu-24.04 (`e2e`); locally:

    python3 tests/passkey_browser.py --devagotchi "$PWD/target/debug/devagotchi"
"""

import argparse
import json
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


def until(check, what: str, seconds: float = 15) -> None:
    """Poll `check` from here: the pages' Content-Security-Policy refuses the eval that
    Playwright's own `wait_for_function` polling needs."""
    deadline = time.time() + seconds
    while not check():
        if time.time() > deadline:
            raise SystemExit(f"timed out waiting until {what}")
        time.sleep(0.05)


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

    work = Path(tempfile.mkdtemp(prefix="passkey-browser-"))
    state, config = work / "state", work / "hub.json"
    port = free_port()
    url = f"http://localhost:{port}"
    config.write_text(
        json.dumps(
            {
                "listen": f"127.0.0.1:{port}",
                "public_url": url,
                "auth": {"mode": "hub"},
                "brokers": [],
                "grants": [],
            }
        )
    )
    hub = subprocess.Popen(
        [args.devagotchi, "hub", "serve", "--state-dir", str(state), "--config", str(config)],
        stdout=subprocess.DEVNULL,
        stderr=open(work / "hub.log", "w"),
    )
    try:
        wait_ready(url, hub)
        made = subprocess.run(
            [args.devagotchi, "hub", "owner", "--state-dir", str(state), "--config", str(config),
             "--name", "Jesse"],
            capture_output=True, text=True, check=True,
        )
        link = made.stdout.strip()
        assert link.startswith(f"{url}/owner#dgl_"), link

        with sync_playwright() as p:
            browser = p.chromium.launch(channel=args.channel)
            page = browser.new_context().new_page()
            cdp = page.context.new_cdp_session(page)
            cdp.send("WebAuthn.enable")
            cdp.send("WebAuthn.addVirtualAuthenticator", {"options": {
                "protocol": "ctap2", "transport": "internal", "hasResidentKey": True,
                "hasUserVerification": True, "isUserVerified": True,
                "automaticPresenceSimulation": True,
            }})

            # 1. The owner's link makes the passkey and signs in.
            page.goto(link)
            until(lambda: not page.evaluate("location.hash"), "the token left the address bar")
            assert page.url == f"{url}/owner", f"the token stayed in the address bar: {page.url}"
            assert page.evaluate("location.hash") == ""
            page.click("#passkey-register")
            page.wait_for_url(f"{url}/account", timeout=15_000)
            text = page.inner_text("main")
            # 2. The account page.
            assert "Signed in as Jesse" in text and "You own this hub" in text, text
            print("PASS owner link made a passkey and signed in")

            # 3. Sign out, then sign in with the passkey alone.
            page.click("header form button")
            page.wait_for_url(f"{url}/signed-out")
            page.goto(f"{url}/account")
            page.wait_for_url(f"{url}/auth/login?return_to=/account")
            page.click("#passkey-sign-in")
            page.wait_for_url(f"{url}/account", timeout=15_000)
            assert "Signed in as Jesse" in page.inner_text("main")
            print("PASS signed out, then back in with the passkey")

            # 4. The spent link no longer works.
            page.goto(link)
            page.click("#passkey-register")
            until(
                lambda: page.inner_text("#passkey-status")
                and not page.inner_text("#passkey-status").startswith(("Making", "Waiting")),
                "the page finished trying the spent link",
            )
            status = page.inner_text("#passkey-status")
            assert "timed out or was already used" in status, status
            print("PASS the spent link is refused")

            # 5. --replace on the device that already holds the owner's passkey: it makes
            # the new one in place of the old, and signs in with it.
            replaced = subprocess.run(
                [args.devagotchi, "hub", "owner", "--state-dir", str(state), "--config",
                 str(config), "--replace"],
                capture_output=True, text=True, check=True,
            ).stdout.strip()
            page.goto(replaced)
            page.click("#passkey-register")
            try:
                page.wait_for_url(f"{url}/account", timeout=15_000)
            except Exception:
                raise SystemExit("--replace did not sign in: " + page.inner_text("#passkey-status"))
            page.click("header form button")
            page.wait_for_url(f"{url}/signed-out")
            page.goto(f"{url}/auth/login")
            page.click("#passkey-sign-in")
            page.wait_for_url(f"{url}/account", timeout=15_000)
            print("PASS --replace works on the device that held the old passkey")
            browser.close()
    finally:
        hub.terminate()
        hub.wait(timeout=10)
    print("PASSKEY BROWSER PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
