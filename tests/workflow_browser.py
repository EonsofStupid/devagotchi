#!/usr/bin/env python3
"""A workflow's link in a real browser: no sign-in, one button, a real machine, a tick in place.

Starts `devagotchi hub serve` as a hosted hub (`tenancy.mode multi`) on SQLite at
http://localhost (a secure context, so WebAuthn works without TLS), handing out a build made
from this binary (`downloads`). Then, as a person and their AI would:

1. An AI client connects (dynamic registration, Connect, PKCE) and calls `workflow_start`
   for "Get started". A plain GET of the link (a chat app's preview) shows the workflow and
   no code, and spends nothing.
2. The link opens in a browser that never signed in, at phone width: the steps, the OS to
   pick (no Mac), no Sign in anywhere, never a `<meta refresh>`.
3. "Get my command" is pressed: the page shows the one line with a Copy button.
4. A real machine enrolls through that line: with `--machine join` the line runs exactly as
   shown (`curl … | sudo sh`, CI); with `--machine link` (the default, for a machine without
   root and systemd) a broker and `devagotchi link enroll` take the code from that line.
5. The page ticks the step in place, without reloading, and offers to keep the account.
   A second AI client joins: the AI starts "Connect ChatGPT", the person presses the page's
   button in this browser, ChatGPT's sign-in page here lands on the same account, and its
   first call ticks that step in place. Then a passkey keeps the account (the Chrome
   DevTools virtual authenticator).
6. `workflow_status` reports the machine step done and the account kept, and the AI runs a
   command on the machine.

Then the same with JavaScript off: Check again shows where it stands.

Needs Python Playwright with its Chromium (tests/requirements-browser.txt). CI runs it on
ubuntu-24.04 (`e2e`); locally:

    python3 tests/workflow_browser.py --devagotchi "$PWD/target/debug/devagotchi"
"""

import argparse
import base64
import hashlib
import io
import json
import os
import re
import secrets
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.parse
import urllib.request
from pathlib import Path

from analytics_browser import start
from clients_oauth import Callback
from operator_browser import ROOT, Person, call, free_port, wait_ready


class Connector:
    """Another AI client connecting the way it does: register, a sign-in page in the person's
    browser (the test drives it), the code back at its loopback redirect, its tokens, calls."""

    def __init__(self, hub: str, name: str):
        self.hub = hub
        self.callback = Callback()
        self.redirect = self.callback.uri
        status, _, body = call(f"{hub}/oauth/register",
                               json.dumps({"client_name": name, "redirect_uris": [self.redirect]}).encode(),
                               {"Content-Type": "application/json"})
        assert status in (200, 201), body
        self.client = json.loads(body)["client_id"]
        self.verifier = secrets.token_urlsafe(48)
        challenge = base64.urlsafe_b64encode(
            hashlib.sha256(self.verifier.encode()).digest()).rstrip(b"=").decode()
        self.authorize_url = f"{hub}/oauth/authorize?" + urllib.parse.urlencode({
            "response_type": "code", "client_id": self.client, "redirect_uri": self.redirect,
            "code_challenge": challenge, "code_challenge_method": "S256", "state": "s9",
            "resource": f"{hub}/mcp",
        })
        self.token = None
        self.session = None

    def finish(self):
        """The code the sign-in page sent back to the client's redirect, for its tokens."""
        code = self.callback.wait()["code"]
        status, _, body = call(f"{self.hub}/oauth/token", urllib.parse.urlencode({
            "grant_type": "authorization_code", "code": code, "redirect_uri": self.redirect,
            "client_id": self.client, "code_verifier": self.verifier,
        }).encode(), {"Content-Type": "application/x-www-form-urlencoded"})
        assert status == 200, body
        self.token = json.loads(body)["access_token"]

    def tool(self, name: str, arguments: dict = None) -> dict:
        return Person.mcp(self, "tools/call", {"name": name, "arguments": arguments or {}})


def downloads(work: Path, binary: str) -> Path:
    """The build the hub hands out: this binary for this machine, and the installer."""
    dist = work / "dist"
    dist.mkdir()
    target = subprocess.run(["bash", str(ROOT / "deploy/linux/install-devagotchi.sh"), "--print-target"],
                            capture_output=True, text=True, check=True).stdout.strip()
    packed = io.BytesIO()
    with tarfile.open(fileobj=packed, mode="w:xz", preset=0) as archive:
        archive.add(binary, arcname="devagotchi")
    (dist / f"devagotchi-{target}.tar.xz").write_bytes(packed.getvalue())
    (dist / "install-devagotchi.sh").write_bytes((ROOT / "deploy/linux/install-devagotchi.sh").read_bytes())
    sums = "".join(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
                   for path in sorted(dist.iterdir()))
    (dist / "SHA256SUMS").write_text(sums)
    (dist / "VERSION").write_text("wf-browser\n")
    return dist


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--devagotchi", required=True, help="absolute path of the binary")
    parser.add_argument("--machine", choices=["link", "join"], default="link",
                        help="join: run the page's line as is (needs sudo and systemd)")
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

    work = Path(tempfile.mkdtemp(prefix="workflow-browser-"))
    state, config = work / "state", work / "hub.json"
    port = free_port()
    url = f"http://localhost:{port}"
    config.write_text(json.dumps({
        "listen": f"127.0.0.1:{port}",
        "public_url": url,
        "auth": {"mode": "hub"},
        "tenancy": {"mode": "multi", "starts_per_hour": 100, "anonymous_connect": True},
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

        # 1. The AI starts "Get started" and gets one link. Previewing it spends nothing.
        ai = Person(url)
        started = ai.tool("workflow_start", {"workflow": "get-started"})
        assert not started.get("isError"), started
        workflow = started["structuredContent"]
        link, wid = workflow["link"], workflow["id"]
        assert link.startswith(f"{url}/go/dgw_"), link
        assert [s["kind"] for s in workflow["steps"]] == ["machine", "done"]
        for _ in range(2):
            status, _, preview = call(link)
            assert status == 200 and "<h1>Get started</h1>" in preview, preview
            assert "dvg-" not in preview and "/join/" not in preview, preview
        listed = ai.tool("machines")["structuredContent"]["machines"]
        assert listed == [], listed
        print("PASS an AI started a workflow; previewing its link shows the steps and no code")

        with sync_playwright() as p:
            browser = p.chromium.launch(channel=args.channel)
            # On a phone the link says first to open it on the computer, with the link to
            # copy and its QR code; nothing about keeping an account.
            phone = browser.new_context(viewport={"width": 390, "height": 844}, user_agent=(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 "
                "(KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1"))
            on_phone = phone.new_page()
            on_phone.goto(link)
            text = on_phone.inner_text("main")
            assert text.strip().startswith("Open this on your computer"), text[:300]
            assert on_phone.locator("#open-on-computer #page-link").inner_text() == link
            assert on_phone.locator("#open-on-computer #qr svg").count() == 1
            assert "Keep your account" not in text, text
            phone.close()
            print("PASS on a phone the link says first to open it on the computer, and nothing about keeping an account")
            # 2. A browser that never signed in, at phone width.
            person = browser.new_context(viewport={"width": 390, "height": 844})
            page = person.new_page()
            cdp = page.context.new_cdp_session(page)
            cdp.send("WebAuthn.enable")
            cdp.send("WebAuthn.addVirtualAuthenticator", {"options": {
                "protocol": "ctap2", "transport": "internal", "hasResidentKey": True,
                "hasUserVerification": True, "isUserVerified": True,
                "automaticPresenceSimulation": True,
            }})
            loads = []
            page.on("load", lambda _: loads.append(page.url))
            page.goto(link)
            html = page.content()
            assert page.locator("h1").inner_text() == "Get started"
            assert "Sign in" not in page.inner_text("body"), page.inner_text("body")
            assert "http-equiv" not in html.lower(), html
            assert page.locator("#step-1").get_attribute("data-state") == "current"
            assert page.locator("input[name=os][value=linux]").is_checked()
            assert page.locator("input[name=os][value=mac]").count() == 0
            assert "not available yet" in page.locator("#step-1 .mac").inner_text()
            assert page.locator("#check").is_hidden(), "the script follows the hub: no Check again"
            width = page.evaluate("document.documentElement.scrollWidth")
            assert width <= 390, f"the page scrolls sideways at phone width: {width}"
            print("PASS the link opens with no sign-in, at phone width, the OS to pick and no Mac")

            # 3. Get my command.
            page.locator("input[name=name]").fill("desk")
            page.locator("#get-command-1").click()
            page.wait_for_url(re.compile(r".*/go/dgw_[^#]*#step-1$"))
            line = page.locator("#command-1").inner_text().strip()
            assert re.fullmatch(rf"curl -fsSL {re.escape(url)}/join/dvg-[0-9a-f]{{32}} \| sudo sh", line), line
            assert page.locator("button.copy[data-copy=command-1]").is_visible()
            code = re.search(r"dvg-[0-9a-f]{32}", line).group(0)
            assert page.locator("#keep").is_hidden(), "nothing to keep before something worked"
            print("PASS Get my command shows the one line, with a Copy button")

            # The page as it is now: nothing below may reload it.
            page.evaluate("window.__stayed = 'here'")
            settled = len(loads)

            # 4. A real machine enrolls through that line.
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

            # 5. The page ticks the step in place and offers to keep the account.
            page.wait_for_selector("#step-1[data-state=done]", timeout=30_000)
            assert page.evaluate("window.__stayed") == "here", "the page reloaded"
            assert len(loads) == settled, loads
            assert page.locator("#step-1 .fact").inner_text() == "desk is online."
            assert page.locator("#step-2").get_attribute("data-state") == "done"
            assert page.locator("#step-1 .step-state").text_content() == "Done"
            page.wait_for_selector("#keep:not([hidden])", timeout=15_000)
            print("PASS the page ticked the machine step without reloading and offers to keep the account")

            # A second AI client: the AI starts "Connect ChatGPT"; the person, in the browser
            # ChatGPT will open, presses the page's button, and ChatGPT's own sign-in there
            # lands on this account. Its first call ticks the step on the open page.
            second = ai.tool("workflow_start", {"workflow": "connect-chatgpt"})["structuredContent"]
            other = person.new_page()
            other.goto(second["link"])
            assert other.locator("#step-1").get_attribute("data-state") == "current"
            assert other.locator("#client-1-0").inner_text() == f"{url}/mcp"
            assert other.locator("#client-1-1").inner_text() == f"codex mcp add devagotchi --url {url}/mcp"
            assert other.locator("button.copy[data-copy=client-1-0]").is_visible()
            other.locator("#client-here-1").click()
            other.wait_for_selector("#client-ready-1")
            other.evaluate("window.__stayed = 'here'")
            chatgpt = Connector(url, "ChatGPT")
            consent = person.new_page()
            consent.goto(chatgpt.authorize_url)
            assert consent.locator("h1").inner_text() == "Connect ChatGPT?"
            assert "your new" not in consent.inner_text("main"), consent.inner_text("main")
            consent.click("button[value=approve]")
            chatgpt.finish()
            me = chatgpt.tool("whoami")
            assert not me.get("isError"), me
            other.wait_for_selector("#step-1[data-state=done]", timeout=30_000)
            assert other.evaluate("window.__stayed") == "here", "the page reloaded"
            assert other.locator("#step-1 .fact").inner_text() == "ChatGPT is connected."
            assert ai.tool("workflow_status", {"id": second["id"]})["structuredContent"]["state"] == "completed"
            consent.close()
            other.close()
            print("PASS a second AI client (ChatGPT) joined the same account from the page's steps; the step ticked in place")

            page.locator("#passkey-add").click()
            page.wait_for_url(re.compile(r".*/go/dgw_[^#]*$"), timeout=15_000)
            page.wait_for_function("!document.getElementById('keep')", timeout=15_000)
            print("PASS a passkey kept the account, from the workflow's page")

            # 6. The AI sees it.
            status = ai.tool("workflow_status", {"id": wid})["structuredContent"]
            states = [s["state"] for s in status["steps"]]
            assert states == ["done", "done"], status
            assert status["steps"][0]["fact"] == "desk is online.", status
            assert status["state"] == "completed", status
            ran = None
            for _ in range(30):
                ran = ai.tool("run", {"machine": "desk", "command": "echo from-the-link"})
                if not ran.get("isError"):
                    break
                time.sleep(0.5)
            assert not ran.get("isError") and "from-the-link" in ran["structuredContent"]["output"], ran
            print("PASS workflow_status reports the machine done and the account kept; the AI ran on it")

            # Without JavaScript: the finished link shows it is done, with no code; an open
            # one's Check again shows where it stands.
            plain = browser.new_context(java_script_enabled=False, viewport={"width": 390, "height": 844})
            other = plain.new_page()
            other.goto(link)
            assert other.locator("#step-1").get_attribute("data-state") == "done"
            assert other.locator("#step-2").get_attribute("data-state") == "done"
            assert code not in other.content()
            linux = ai.tool("workflow_start", {"workflow": "connect-linux"})["structuredContent"]["link"]
            other.goto(linux)
            assert other.locator("#check button").is_visible()
            other.locator("#check button").click()
            assert other.locator("#step-1").get_attribute("data-state") == "current"
            print("PASS without JavaScript, Check again shows where it stands")
            browser.close()
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
    print("WORKFLOW BROWSER PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
