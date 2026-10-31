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

Needs Python Playwright with its Chromium. Not run in CI (the runners have no Playwright);
run it before pushing a change to the sign-in pages:

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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--devagotchi", required=True, help="absolute path of the binary")
    args = parser.parse_args()
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        print("SKIPPED: Python Playwright is not installed")
        return 0

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
            browser = p.chromium.launch()
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
            assert page.url == f"{url}/owner", f"the token stayed in the address bar: {page.url}"
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
            page.wait_for_function(
                "document.getElementById('passkey-status').textContent.length > 0"
                " && !document.getElementById('passkey-status').textContent.startsWith('Making')"
                " && !document.getElementById('passkey-status').textContent.startsWith('Waiting')"
            )
            status = page.inner_text("#passkey-status")
            assert "timed out or was already used" in status, status
            print("PASS the spent link is refused")
            browser.close()
    finally:
        hub.terminate()
        hub.wait(timeout=10)
    print("PASSKEY BROWSER PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
