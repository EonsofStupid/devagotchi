#!/usr/bin/env python3
"""The phone control app in a real browser (docs/spec-phone-app.md, Tests), at phone size.

Starts `devagotchi hub serve` as a hosted hub (`tenancy.mode multi`) on SQLite at
http://localhost (a secure context, so the service worker runs without TLS). Then:

1. A person signs up from their AI client's sign-in page on an iPhone 13 (emulated); the AI
   adds a machine (a broker and `devagotchi link`, online) and a board with a ticket.
2. `/app` on the iPhone shows the machine online and the board, in one frame that does not
   scroll sideways at 390 px; the install banner says Share, then Add to Home Screen.
   Its machine page: online, its build, last seen now, the AI's command among the recent
   jobs, and Update now saying it is already on the hub's build (the hub hands out this
   binary's own build).
3. The manifest starts the installed app at `/app`, standalone.
4. The same on a Pixel 7 (emulated), signed in there with the email and password.
5. With the hub gone, reloading the app shows its offline page, not a browser error.

Needs Python Playwright with its Chromium (tests/requirements-browser.txt). CI runs it on
ubuntu-24.04 (`e2e`, beside tests/workflow_browser.py); locally:

    python3 tests/app_browser.py --devagotchi "$PWD/target/debug/devagotchi"
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
from operator_browser import call, free_port, wait_ready
from workflow_browser import Connector, downloads

EMAIL, PASSWORD = "phone@example.com", "a good password"


def device(p, name: str) -> dict:
    """Playwright's descriptor of a phone, for Chromium."""
    described = dict(p.devices[name])
    described.pop("default_browser_type", None)
    return described


def link_machine(binary: str, url: str, line: str, work: Path, processes: list) -> None:
    """A machine without root: a broker and `devagotchi link enroll` with the line's code."""
    code = re.search(r"dvg-[0-9a-f]{32}", line).group(0)
    script = urllib.request.urlopen(f"{url}/join/{code}", timeout=10).read().decode()
    name = re.search(r"^device='([a-z0-9-]+)'$", script, re.M).group(1)
    workspace = work / "workspace"
    workspace.mkdir()
    (work / "broker.json").write_text(json.dumps({
        "workspace": str(workspace), "exec_policy": "account",
        "output_limit_bytes": 65536, "max_operations": 16, "max_concurrent": 2,
    }))
    broker, ready = start([binary, "broker", "run", "--state-dir", str(work / "broker-state"),
                           "--config", str(work / "broker.json")], "broker", work)
    processes.append(broker)
    enrolled = subprocess.run(
        [binary, "link", "enroll", "--hub", url, "--device", name,
         "--broker", ready.split("ready:", 1)[1].strip(), "--state-dir", str(work / "link-state")],
        capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL,
        env={**os.environ, "DEVAGOTCHI_ENROLL_CODE": code},
    )
    assert enrolled.returncode == 0, enrolled.stderr
    linked, _ = start([binary, "link", "run", "--state-dir", str(work / "link-state")], "link", work)
    processes.append(linked)


def no_sideways_scroll(page, width: int) -> None:
    for what in ("document.documentElement.scrollWidth", "document.querySelector('body.app > main').scrollWidth"):
        seen = page.evaluate(what)
        assert seen <= width, f"{what} is {seen} at {width} px: the page scrolls sideways"


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

    work = Path(tempfile.mkdtemp(prefix="app-browser-"))
    state, config = work / "state", work / "hub.json"
    port = free_port()
    url = f"http://localhost:{port}"
    # The hub hands out this binary's own build, so Update now finds the machine on it.
    build = re.search(r"\(([^)]+)\)", subprocess.run([args.devagotchi, "--version"], capture_output=True,
                                                     text=True, check=True).stdout).group(1)
    dist = downloads(work, args.devagotchi)
    (dist / "VERSION").write_text(build + "\n")
    config.write_text(json.dumps({
        "listen": f"127.0.0.1:{port}",
        "public_url": url,
        "auth": {"mode": "hub"},
        "tenancy": {"mode": "multi", "starts_per_hour": 100},
        "account_boards": {"dir": str(work / "boards"), "free_limit": 5},
        "downloads": str(dist),
        "machines": {"auto_update": False},
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
    try:
        wait_ready(url, hub)
        ai = Connector(url, "Claude")
        with sync_playwright() as p:
            browser = p.chromium.launch(channel=args.channel)
            iphone = browser.new_context(**device(p, "iPhone 13"))
            page = iphone.new_page()

            # 1. Sign up from the AI's sign-in page; the AI adds a machine and a board.
            page.goto(ai.authorize_url)
            page.locator("a[href*='tab=up']").click()
            page.locator("input[name=email]").fill(EMAIL)
            page.locator("input[name=password]").fill(PASSWORD)
            page.locator("form[action='/auth/password/new'] button[type=submit]").click()
            page.wait_for_url(re.compile(re.escape(ai.redirect) + r"\?.*code="), timeout=15_000)
            ai.finish()
            added = ai.tool("machine_add", {"name": "desk"})
            assert not added.get("isError"), added
            link_machine(args.devagotchi, url, added["structuredContent"]["linux"], work, processes)
            for _ in range(60):
                listed = ai.tool("machines")["structuredContent"]["machines"]
                if any(m["name"] == "desk" and m["status"] == "online" for m in listed):
                    break
                time.sleep(0.5)
            else:
                raise AssertionError(f"desk never came online: {listed}")
            bound = ai.tool("board_bind", {"project": "alpha"})
            assert not bound.get("isError"), bound
            made = ai.tool("board_create", {"project": "alpha", "title": "Check the backups"})
            assert not made.get("isError"), made
            ran = ai.tool("run", {"machine": "desk", "command": "echo from-the-phone"})
            assert not ran.get("isError"), ran
            print("PASS signed up on an iPhone; the AI added desk (online) and the board alpha")

            # 2. Home on the iPhone: the machine and the board, one frame, no sideways scroll.
            page.goto(f"{url}/app")
            assert urllib.parse.urlsplit(page.url).path == "/app", page.url
            machine = page.locator("#machines #machine-desk")
            assert "Online" in machine.inner_text(), machine.inner_text()
            assert "alpha" in page.locator("#boards").inner_text()
            assert page.locator("nav.tabs a[aria-current=page]").inner_text() == "Home"
            no_sideways_scroll(page, 390)
            banner = page.locator("#install [data-os=iphone]")
            banner.wait_for(state="visible", timeout=5_000)
            assert "Add to Home Screen" in banner.inner_text()
            print("PASS /app on an iPhone 13 shows desk online and alpha; no sideways scroll at 390 px")

            # Its machine page: status, build, last seen, recent jobs, Update now.
            page.locator("#machine-desk a").click()
            page.wait_for_url(re.compile(r".*/app/machines/desk$"))
            assert page.locator("h1").inner_text() == "desk"
            assert "Online" in page.locator("#machine").inner_text()
            assert page.locator("#build").inner_text() == build
            assert page.locator("#last-seen").inner_text() == "Now"
            jobs = page.locator("table.jobs").inner_text()
            assert "echo from-the-phone" in jobs and "succeeded" in jobs, jobs
            no_sideways_scroll(page, 390)
            page.locator("#update-now").click()
            page.wait_for_url(re.compile(r".*/app/machines/desk\?state=done.*"))
            result = page.locator("#update-result").inner_text()
            assert "Update: done" in result and f"already on {build}" in result, result
            print("PASS desk's page: online, its build, last seen now, the AI's job, Update now answered")

            # 3. The installed app starts at /app, standalone.
            status, _, body = call(f"{url}/manifest.webmanifest")
            manifest = json.loads(body)
            assert status == 200 and manifest["start_url"] == "/app", manifest
            assert manifest["display"] == "standalone", manifest
            print("PASS the manifest has start_url /app and display standalone")

            # 4. A Pixel 7, signed in with the email and password.
            pixel = browser.new_context(**device(p, "Pixel 7"))
            android = pixel.new_page()
            android.goto(f"{url}/app")
            assert urllib.parse.urlsplit(android.url).path == "/auth/login", android.url
            android.locator("form[action='/auth/password'] input[name=email]").fill(EMAIL)
            android.locator("form[action='/auth/password'] input[name=password]").fill(PASSWORD)
            android.locator("form[action='/auth/password'] button[type=submit]").click()
            android.wait_for_url(re.compile(r".*/app$"), timeout=15_000)
            assert "Online" in android.locator("#machine-desk").inner_text()
            assert "alpha" in android.locator("#boards").inner_text()
            no_sideways_scroll(android, android.viewport_size["width"])
            print("PASS /app on a Pixel 7 after signing in there")

            # 5. Offline: the service worker keeps the frame; with the hub gone a reload
            # shows the app's offline page instead of a browser error.
            page.goto(f"{url}/app")
            page.wait_for_function("navigator.serviceWorker && navigator.serviceWorker.controller",
                                   timeout=15_000)
            page.wait_for_function(
                "caches.open('devagotchi-app-1').then(c => c.match('/app/offline')).then(Boolean)",
                timeout=15_000)
            hub.terminate()
            hub.wait(timeout=10)
            page.reload()
            assert page.locator("h1").inner_text() == "You're offline", page.content()
            assert page.locator("nav.tabs").count() == 1
            no_sideways_scroll(page, 390)
            print("PASS with the hub gone, reloading the app shows its offline page")
            browser.close()
    finally:
        for process in reversed(processes):
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
    print("APP BROWSER PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
