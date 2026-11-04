#!/usr/bin/env python3
"""Is the live hub working? Checks a deployed hub from the outside, the way an AI app and a
person reach it, and says plainly what works and what does not.

    python3 tests/live_check.py --hub https://app.example.com [--site https://example.com]
        [--client dgc_...] [--token <access token>] [--expect-version main-abc1234]
        [--json receipt.json]

Without --token it only reads: health, the MCP front door (401 naming its metadata), the
OAuth metadata, the authorize page an app's Connect opens, the join script, the builds, and
the public pages and their files for AI agents and crawlers. It never presses Connect and
never makes an account. Without --client it registers one OAuth client for the authorize
page (an app does the same on its first connect); pass the printed id next time to reuse it.
With --token (an account's access token for --hub) it also opens an MCP session and calls
read-only tools: whoami, machines, workflow_list. Exit 0 only when every check passed.
"""

import argparse
import base64
import hashlib
import html
import json
import re
import secrets
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

RESULTS = []
# Python's default User-Agent is refused by some edges (Cloudflare answers 403); say who we are.
AGENT = {"user-agent": "devagotchi-live-check/1"}


def check(name, ok, detail=""):
    RESULTS.append({"check": name, "ok": bool(ok), "detail": detail})
    print(f"{'PASS' if ok else 'FAIL'} {name}{': ' + detail if detail else ''}", flush=True)
    return ok


def fetch(url, data=None, headers=None, method=None):
    """(status, headers, body text); never raises on an HTTP status."""
    request = urllib.request.Request(url, data=data, headers={**AGENT, **(headers or {})},
                                     method=method)
    try:
        with urllib.request.urlopen(request, timeout=30) as answer:
            return answer.status, answer.headers, answer.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read().decode("utf-8", "replace")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def text_of(page):
    page = re.sub(r"<script.*?</script>|<style.*?</style>", " ", page, flags=re.S)
    return re.sub(r"\s+", " ", html.unescape(re.sub(r"<[^>]+>", " ", page))).strip()


def front_door(hub, client, expect_version):
    status, _, body = fetch(f"{hub}/healthz")
    health = json.loads(body) if status == 200 else {}
    check("health answers ok", status == 200 and health.get("ok") is True, f"HTTP {status}")
    downloads = health.get("downloads") or {}
    check("the hub hands out builds", downloads.get("files", 0) >= 6, json.dumps(downloads))
    if expect_version:
        check("it runs the expected build", downloads.get("version") == expect_version,
              f"{downloads.get('version')} (expected {expect_version})")
    init = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "live-check", "version": "1"}}}).encode()
    status, headers, _ = fetch(f"{hub}/mcp", init, {
        "content-type": "application/json", "accept": "application/json, text/event-stream"})
    challenge = headers.get("www-authenticate", "") if headers else ""
    check("/mcp asks an app to sign in (401 naming its metadata)",
          status == 401 and "resource_metadata=" in challenge, f"HTTP {status} {challenge}")
    status, _, body = fetch(f"{hub}/.well-known/oauth-protected-resource")
    resource = json.loads(body) if status == 200 else {}
    check("protected-resource metadata names this hub",
          hub in resource.get("authorization_servers", []), resource.get("resource", ""))
    status, _, body = fetch(f"{hub}/.well-known/oauth-authorization-server")
    meta = json.loads(body) if status == 200 else {}
    needed = ["authorization_endpoint", "token_endpoint", "registration_endpoint",
              "device_authorization_endpoint"]
    check("OAuth metadata has every endpoint an app needs", all(meta.get(k) for k in needed),
          ", ".join(k for k in needed if not meta.get(k)) or "all present")
    if not client and meta.get("registration_endpoint"):
        status, _, body = fetch(meta["registration_endpoint"], json.dumps({
            "client_name": "devagotchi live check (read-only)",
            "redirect_uris": ["http://127.0.0.1:9/cb"], "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"]}).encode(), {"content-type": "application/json"})
        client = json.loads(body).get("client_id", "") if status in (200, 201) else ""
        check("an app can register itself", bool(client), f"HTTP {status}; reuse --client {client}")
    if client:
        challenge = base64.urlsafe_b64encode(hashlib.sha256(
            secrets.token_urlsafe(48).encode()).digest()).rstrip(b"=").decode()
        query = urllib.parse.urlencode({
            "response_type": "code", "client_id": client, "scope": "mcp", "state": "live",
            "redirect_uri": "http://127.0.0.1:9/cb", "code_challenge": challenge,
            "code_challenge_method": "S256", "resource": hub})
        status, _, page = fetch(f"{meta.get('authorization_endpoint', hub + '/oauth/authorize')}?{query}")
        words = text_of(page)
        hosted = "makes yours" in words or "no account here yet" in words
        check("Connect opens a page with one Connect button", status == 200 and "Connect" in words,
              "hosted: a new person's account is made on Connect, no sign-up" if hosted
              else "owner-only: only the owner can sign in here")
    return client


def joining(hub):
    status, _, body = fetch(f"{hub}/join/dvg-{'0' * 32}")
    lines = body.splitlines()
    check("the join line answers with a script", status == 200 and body.startswith("#!"),
          lines[1][:120] if len(lines) > 1 else f"HTTP {status}")
    status, _, body = fetch(f"{hub}/dist/SHA256SUMS")
    names = [line.split()[-1] for line in body.splitlines() if line.strip()] if status == 200 else []
    check("the builds are listed with their sums", len(names) >= 6, ", ".join(names))


def public(site, hub):
    status, _, page = fetch(f"{site}/")
    check("the landing page loads", status == 200, f"HTTP {status}")
    check("no other product is named", "desktop commander" not in page.lower())
    canonical = re.search(r'<link rel="canonical" href="([^"]+)"', page)
    check("the landing page names its canonical address", bool(canonical),
          canonical.group(1) if canonical else "none")
    opener = urllib.request.build_opener(NoRedirect)
    for path, must in [("/llms.txt", f"{hub}/mcp"), ("/robots.txt", "Disallow: /account"),
                       ("/sitemap.xml", "<urlset")]:
        try:
            with opener.open(urllib.request.Request(f"{site}{path}", headers=AGENT),
                             timeout=30) as answer:
                status, body = answer.status, answer.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as error:
            status, body = error.code, ""
        check(f"{path} is served at {site}", status == 200 and must in body, f"HTTP {status}")
    status, _, robots = fetch(f"{site}/robots.txt")
    blocked = [bot for bot in ["ClaudeBot", "GPTBot", "Google-Extended", "PerplexityBot",
                               "OAI-SearchBot", "Claude-SearchBot"]
               if re.search(rf"User-agent: {bot}\s*\nDisallow: /\s*\n", robots)]
    check("AI crawlers may read the public pages", not blocked,
          "blocked: " + ", ".join(blocked) if blocked else "none blocked")


def mcp_session(hub, token):
    headers = {"content-type": "application/json", "accept": "application/json, text/event-stream",
               "authorization": f"Bearer {token}"}

    def rpc(method, params, ident, session=None):
        extra = {"mcp-session-id": session} if session else {}
        status, answer, body = fetch(f"{hub}/mcp", json.dumps({
            "jsonrpc": "2.0", "id": ident, "method": method, "params": params}).encode(),
            {**headers, **extra})
        for line in body.splitlines():
            if line.startswith("data:"):
                body = line[5:].strip()
        return status, (answer.get("mcp-session-id") if answer else None), json.loads(body or "{}")

    status, session, init = rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                               "clientInfo": {"name": "live-check", "version": "1"}}, 1)
    if not check("an app with a token opens a session", status == 200 and "result" in init,
                 f"HTTP {status}"):
        return
    fetch(f"{hub}/mcp", json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}).encode(),
          {**headers, **({"mcp-session-id": session} if session else {})})
    _, _, listed = rpc("tools/list", {}, 2, session)
    tools = {tool["name"] for tool in listed.get("result", {}).get("tools", [])}
    want = {"whoami", "machines", "machine_add", "workflow_start", "workflow_status", "run"}
    check("the tools an AI needs are offered", want <= tools, ", ".join(sorted(want - tools)) or f"{len(tools)} tools")
    for ident, name in enumerate(["whoami", "machines", "workflow_list"], start=3):
        _, _, called = rpc("tools/call", {"name": name, "arguments": {}}, ident, session)
        result = called.get("result", {})
        check(f"{name} answers", "result" in called and not result.get("isError"),
              json.dumps(result.get("structuredContent", result))[:200])


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--hub", required=True, help="the hub's public URL (where /mcp is)")
    parser.add_argument("--site", help="the marketing site, if the public pages live elsewhere")
    parser.add_argument("--client", help="an OAuth client id this check registered before")
    parser.add_argument("--token", help="an account's access token for the hub (optional)")
    parser.add_argument("--expect-version", help="the build the hub should run, e.g. main-abc1234")
    parser.add_argument("--json", help="write the results here as a receipt")
    args = parser.parse_args()
    hub, site = args.hub.rstrip("/"), (args.site or args.hub).rstrip("/")
    started = time.time()
    client = front_door(hub, args.client, args.expect_version)
    joining(hub)
    public(site, hub)
    if args.token:
        mcp_session(hub, args.token)
    failed = [r for r in RESULTS if not r["ok"]]
    print(f"\n{len(RESULTS) - len(failed)}/{len(RESULTS)} passed against {hub}", flush=True)
    if args.json:
        with open(args.json, "w", encoding="utf-8") as out:
            json.dump({"hub": hub, "site": site, "client": client, "at": int(started),
                       "results": RESULTS}, out, indent=2)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
