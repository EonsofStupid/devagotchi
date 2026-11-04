#!/usr/bin/env python3
"""First connect in a real browser (docs/spec-first-connect.md, acceptance 1).

Starts `devagotchi hub serve` as a hosted hub (`tenancy.mode multi`, no `anonymous_connect`)
on SQLite at http://localhost, handing out a build made from this binary (`downloads`). Then,
as a person and their AI would:

1. An AI client registers (DCR) and opens its authorize URL in a browser that never signed
   in: the hub's sign-in page, with only the two choices (Continue with a provider, or an
   email and a password) and nothing about passkeys, securing or Connect.
2. Sign up there. The very next navigation is the client's `redirect_uri` with a code: the
   person saw exactly 2 hub pages (the sign-in page they opened, and the one they submitted),
   no Connect page.
3. The client's tokens; MCP `whoami` has no `secure_url`.
4. `machine_add`, and its Linux line run (`--machine join`: as is, `curl … | sudo sh`, CI;
   `--machine link`, the default: a broker and `devagotchi link enroll` with its code).
5. `machines` shows it online under its shown name, and `run echo ok` returns `ok`.

Needs Python Playwright with its Chromium (tests/requirements-browser.txt). CI runs it on
ubuntu-24.04 (`e2e`); locally:

    python3 tests/first_connect_browser.py --devagotchi "$PWD/target/debug/devagotchi"
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from pathlib import Path

from analytics_browser import start
from operator_browser import ROOT, free_port, wait_ready
from workflow_browser import Connector, downloads


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--devagotchi", required=True, help="absolute path of the binary")
    parser.add_argument("--machine", choices=["link", "join"], default="link",
                        help="join: run the Linux line as is (needs sudo and systemd)")
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

    work = Path(tempfile.mkdtemp(prefix="first-connect-"))
    state, config = work / "state", work / "hub.json"
    port = free_port()
    url = f"http://localhost:{port}"
    config.write_text(json.dumps({
        "listen": f"127.0.0.1:{port}",
        "public_url": url,
        "auth": {"mode": "hub"},
        "tenancy": {"mode": "multi", "starts_per_hour": 100},
        "downloads": str(downloads(work, args.devagotchi)),
        "brokers": [],
        "grants": [],
    }))
    processes = []
    hub = subprocess.Popen(
        [args.devagotchi, "hub", "serve", "--state-dir", str(state), "--config", str(config)],
        stdout=subprocess.DEVNULL,
        stderr=open(work / "hub.log", "w"),
    )
    processes.append(hub)
    joined = False
    try:
        wait_ready(url, hub)
        ai = Connector(url, "Claude")
        with sync_playwright() as p:
            browser = p.chromium.launch(channel=args.channel)
            page = browser.new_context().new_page()
            seen = []
            page.on("framenavigated", lambda frame: frame == page.main_frame and seen.append(frame.url))

            # 1. The authorize URL, no session: the sign-in page and its two choices only.
            page.goto(ai.authorize_url)
            assert urllib.parse.urlsplit(page.url).path == "/auth/login", page.url
            text = page.inner_text("main")
            html = page.content().lower()
            assert page.locator("form[action='/auth/password'] input[name=email]").count() == 1
            assert page.locator("form[action='/auth/password'] input[name=password]").count() == 1
            assert "Sign up" in text and "Forgot password?" in text, text
            for word in ("passkey", "secure", "connect"):
                assert word not in html, f"the sign-in page says {word!r}"
            print("PASS the authorize URL with no session opens the sign-in page: email and password, nothing else")

            # 2. Sign up: the very next navigation is the client's redirect_uri.
            page.locator("a[href*='tab=up']").click()
            page.locator("input[name=email]").fill("first@example.com")
            page.locator("input[name=password]").fill("a good password")
            page.locator("form[action='/auth/password/new'] button[type=submit]").click()
            page.wait_for_url(re.compile(re.escape(ai.redirect) + r"\?.*code="), timeout=15_000)
            hub_pages = [u for u in seen if u.startswith(url)]
            assert len(hub_pages) == 2, hub_pages
            assert seen[-1].startswith(ai.redirect), seen
            print("PASS sign up went straight back to the app with a code: exactly 2 hub pages, no Connect")
            browser.close()

        # 3. Tokens, and whoami: nothing to secure.
        ai.finish()
        me = ai.tool("whoami")
        assert not me.get("isError"), me
        assert "secure_url" not in me["structuredContent"], me
        print("PASS the app's token works and whoami has no secure_url")

        # 4. machine_add, and its Linux line.
        added = ai.tool("machine_add", {"name": "desk"})
        assert not added.get("isError"), added
        line = added["structuredContent"]["linux"]
        code = re.search(r"dvg-[0-9a-f]{32}", line).group(0)
        if args.machine == "join":
            ran = subprocess.run(["bash", "-c", line], stdin=subprocess.DEVNULL,
                                 capture_output=True, text=True, timeout=600)
            print(ran.stderr[-3000:], file=sys.stderr)
            assert ran.returncode == 0, ran.stdout[-2000:]
            joined = True
        else:
            script = urllib.request.urlopen(f"{url}/join/{code}", timeout=10).read().decode()
            device = re.search(r"^device='([a-z0-9-]+)'$", script, re.M).group(1)
            workspace = work / "workspace"
            workspace.mkdir()
            (work / "broker.json").write_text(json.dumps({
                "workspace": str(workspace), "exec_policy": "account",
                "output_limit_bytes": 65536, "max_operations": 16, "max_concurrent": 2,
            }))
            broker, ready = start([args.devagotchi, "broker", "run", "--state-dir",
                                   str(work / "broker-state"), "--config",
                                   str(work / "broker.json")], "broker", work)
            processes.append(broker)
            endpoint = ready.split("ready:", 1)[1].strip()
            enrolled = subprocess.run(
                [args.devagotchi, "link", "enroll", "--hub", url, "--device", device,
                 "--broker", endpoint, "--state-dir", str(work / "link-state")],
                capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL,
                env={**os.environ, "DEVAGOTCHI_ENROLL_CODE": code},
            )
            assert enrolled.returncode == 0, enrolled.stderr
            linked, _ = start([args.devagotchi, "link", "run", "--state-dir",
                               str(work / "link-state")], "link", work)
            processes.append(linked)
        print("PASS machine_add's Linux line ran")

        # 5. Online under its shown name, and it runs.
        online = []
        for _ in range(60):
            listed = ai.tool("machines")["structuredContent"]["machines"]
            online = [m for m in listed if m.get("name") == "desk" and m.get("status") == "online"]
            if online:
                break
            time.sleep(0.5)
        assert online, listed
        ran = None
        for _ in range(30):
            ran = ai.tool("run", {"machine": "desk", "command": "echo ok"})
            if not ran.get("isError"):
                break
            time.sleep(0.5)
        assert not ran.get("isError") and ran["structuredContent"]["output"].strip() == "ok", ran
        print("PASS machines shows desk online; run echo ok returns ok")
    finally:
        for process in reversed(processes):
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
        if joined:
            subprocess.run(["sudo", "bash", str(ROOT / "deploy/linux/install-devagotchi.sh"), "--uninstall"],
                           stdin=subprocess.DEVNULL, capture_output=True, timeout=120)
    print("FIRST CONNECT BROWSER PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
