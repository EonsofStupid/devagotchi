#!/usr/bin/env python3
"""The operator's analytics (`/operator/analytics`) in a real browser: counts and the funnel move.

Starts `devagotchi hub serve` as a hosted hub (`tenancy.mode multi`, sign-up open) on SQLite
at http://localhost (a secure context, so WebAuthn works without TLS). Then:

1. The operator signs in with a passkey (`devagotchi hub owner`'s link, the Chrome DevTools
   virtual authenticator). Their analytics page, with JavaScript off, counts nothing yet.
2. A visitor's browser (a person's User-Agent, JavaScript off) opens three public pages, one
   from a campaign link. A person's AI client connects (dynamic registration, Connect,
   PKCE), adds a machine with `machine_add`; a broker and its `devagotchi link` enroll it
   with the code; the AI runs a command on it with `run`.
3. The analytics page and its funnel moved: one visitor and three page views today, the
   campaign, one account made by Claude and let in, its first machine and first command, one
   tenant active. The CSV holds today's totals. Nobody but the operator gets anything (404),
   and the operator's own visits are not counted.

Needs Python Playwright with its Chromium (tests/requirements-browser.txt). CI runs it on
ubuntu-24.04 (`e2e`); locally:

    python3 tests/analytics_browser.py --devagotchi "$PWD/target/debug/devagotchi"
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

from operator_browser import Person, call, free_port, wait_ready

# A person's browser. Headless Chrome names itself in its User-Agent, and robots are not
# counted, so the visitor's browser says what a person's would.
PERSON_UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) "
             "Chrome/141.0.0.0 Safari/537.36")


def start(argv, name, work, env=None):
    """Start a process and wait for its `ready:` line: the process and that line."""
    process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                               stderr=subprocess.PIPE, text=True,
                               env={**os.environ, **(env or {})})
    ready, seen = threading.Event(), {}
    log = open(work / f"{name}.log", "w")

    def pump():
        for line in process.stderr:
            log.write(line)
            log.flush()
            if "ready:" in line and "line" not in seen:
                seen["line"] = line.strip()
                ready.set()
        ready.set()

    threading.Thread(target=pump, daemon=True).start()
    if not ready.wait(30) or "line" not in seen:
        raise SystemExit(f"{name} did not become ready (exit {process.poll()}); see {work}/{name}.log")
    return process, seen["line"]


def step(text: str, name: str) -> list:
    """The funnel row `name` as the page shows it: its count and its rate."""
    for line in text.splitlines():
        cells = line.split("\t")
        if len(cells) >= 3 and cells[0].strip() == name:
            return [cell.strip() for cell in cells[1:3]]
    raise AssertionError(f"no funnel row {name!r} in:\n{text}")


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

    work = Path(tempfile.mkdtemp(prefix="analytics-browser-"))
    state, config = work / "state", work / "hub.json"
    port = free_port()
    url = f"http://localhost:{port}"
    config.write_text(json.dumps({
        "listen": f"127.0.0.1:{port}",
        "public_url": url,
        "auth": {"mode": "hub"},
        "tenancy": {"mode": "multi", "starts_per_hour": 100, "anonymous_connect": True},
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
        link = subprocess.run(
            [args.devagotchi, "hub", "owner", "--state-dir", str(state), "--config", str(config),
             "--name", "Jesse"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        assert link.startswith(f"{url}/owner#dgl_"), link

        with sync_playwright() as p:
            browser = p.chromium.launch(channel=args.channel)
            # 1. The operator signs in with a passkey; nothing is counted yet.
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
            operator = browser.new_context(java_script_enabled=False,
                                           viewport={"width": 390, "height": 900},
                                           user_agent=PERSON_UA)
            operator.add_cookies(signing.cookies())
            board = operator.new_page()
            board.goto(f"{url}/operator")
            board.get_by_role("link", name="analytics").click()
            board.wait_for_url(f"{url}/operator/analytics")
            text = board.inner_text("main")
            assert step(text, "Visitors") == ["0", "—"], text
            assert step(text, "Accounts made") == ["0", "—"], text
            print("PASS the operator page leads to analytics, with nothing counted yet")

            # 2. A visitor reads three pages, one from a campaign link.
            visitor = browser.new_context(java_script_enabled=False, user_agent=PERSON_UA)
            reading = visitor.new_page()
            for path in ["/?utm_source=newsletter&utm_medium=email&utm_campaign=launch",
                         "/docs", "/privacy"]:
                answer = reading.goto(f"{url}{path}")
                assert answer.status == 200, (path, answer.status)
            # The operator reading public pages is not counted.
            board.goto(f"{url}/docs")

            # A person connects Claude, adds a machine and runs a command on it.
            person = Person(url)
            added = person.tool("machine_add", {"name": "desk"})
            assert not added.get("isError"), added
            machine = added["structuredContent"]
            device = re.search(r"--device (\S+)", machine["manual"]).group(1)
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
                env={**os.environ, "DEVAGOTCHI_ENROLL_CODE": machine["code"]},
            )
            assert enrolled.returncode == 0, enrolled.stderr
            linked, _ = start([args.devagotchi, "link", "run", "--state-dir",
                               str(work / "link-state")], "link", work)
            processes.append(linked)
            ran = None
            for _ in range(30):
                ran = person.tool("run", {"machine": "desk", "command": "echo counted"})
                if not ran.get("isError"):
                    break
                time.sleep(0.5)
            assert not ran.get("isError") and "counted" in ran["structuredContent"]["output"], ran
            print("PASS a visitor read three pages; a person connected, enrolled a machine and ran on it")

            # 3. Nobody but the operator gets the page or its CSV.
            for path in ["/operator/analytics", "/operator/analytics.csv?days=7"]:
                assert call(f"{url}{path}")[:3:2] == (404, ""), path
                assert call(f"{url}{path}", headers={"Cookie": person.cookie})[0] == 404, path
            print("PASS the analytics page and its CSV are a 404 to anyone else")

            # The counts and the funnel moved (the writer batches for a second).
            time.sleep(1.5)
            board.goto(f"{url}/operator/analytics?days=7")
            text = board.inner_text("main")
            assert board.locator("nav.range a[aria-current='page']").inner_text() == "7 days"
            assert "3 page views and 1 visitors over 7 days" in board.locator(
                "svg[role=img] desc").text_content(), text
            assert step(text, "Visitors") == ["1", "—"], text
            assert step(text, "Accounts made") == ["1", "100.0%"], text
            assert step(text, "Let in")[0] == "1", text
            assert step(text, "First machine") == ["1", "100.0%"], text
            assert step(text, "First command run") == ["1", "100.0%"], text
            for want in ["newsletter", "email", "launch", "/docs", "/privacy", "Computer"]:
                assert want in text, (want, text)
            assert board.locator("#clients tr", has_text="Claude").inner_text().split()[-1] == "1"
            assert "1 in the last 7 days" in board.locator("#active").inner_text()
            assert "Waiting for early access now: 0." in text, text
            print("PASS the operator sees the visits, the campaign and the funnel move")

            # The CSV: today's totals.
            with board.expect_download() as download:
                board.locator("#export").click()
            csv = Path(download.value.path()).read_text()
            assert csv.startswith("day,metric,item,views,visitors"), csv
            assert re.search(r"^\d{4}-\d\d-\d\d,total,,3,1\r?$", csv, re.M), csv
            assert re.search(r",event,operation,1,0\r?$", csv, re.M), csv
            print("PASS the daily totals download as CSV")
            browser.close()
    finally:
        for process in reversed(processes):
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
    print("ANALYTICS BROWSER PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
