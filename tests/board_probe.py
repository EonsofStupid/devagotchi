#!/usr/bin/env python3
"""Probe a running hub's board tools over Streamable HTTP MCP.

    python3 tests/board_probe.py http://127.0.0.1:8787 --project e2e-board --dir /path/to/project \
        --devagotchi target/debug/devagotchi \
        --tokens operator=<bearer> claude=<bearer> codex=<bearer> reader=<bearer>

Three subjects work one project's board the way three subscriptions would: a person
(the hub's owner, or a grant marked human) creates tickets, an agent claims the ready
one, heartbeats without restating its fence, notes what it found, files the work it
discovered, completes into review; the same agent is refused the review (the reviewer
is never the author); a second agent reviews it to done; a person force-releases a
held ticket and the agent's stale fence is refused; an ungranted subject is refused
with `not_granted` and an unknown project with `unknown_board`; events, long-poll
wait and the hash chain answer; and the command line, run against the same directory,
sees the tickets the tools made. Then account boards (D23, packet 15a): the ungranted
subject binds its own project, works it from two seats of the one account (a claim on one
seat is held against the other), hits the free limit on a second project, and the shared
board's name cannot be bound by someone who reaches it. Exit 0 only when every step holds.
"""
import argparse
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument("base")
parser.add_argument("--project", required=True)
parser.add_argument("--dir", required=True, help="the project directory the hub serves")
parser.add_argument("--devagotchi", required=True, help="the one binary, for `devagotchi board ls` against the same directory")
parser.add_argument("--tokens", nargs="+", required=True, help="name=bearer, for operator claude codex reader")
args = parser.parse_args()
base = args.base.rstrip("/")
tokens = dict(pair.split("=", 1) for pair in args.tokens)
for who in ("operator", "claude", "codex", "reader"):
    if who not in tokens:
        sys.exit(f"--tokens needs {who}=<bearer>")
failures = 0


def check(name, ok, detail=""):
    global failures
    print(f"{'ok  ' if ok else 'FAIL'} {name} {detail}".rstrip(), flush=True)
    if not ok:
        failures += 1


class Client:
    """One MCP session per subject, as each agent would hold its own."""

    def __init__(self, who, seat=None):
        self.who = who if seat is None else f"{who} as {seat}"
        self.token = tokens[who]
        self.seat = seat
        self.session = None
        self.rid = 0
        init = self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                       "clientInfo": {"name": f"board-probe-{who}", "version": "0"}})
        assert init["serverInfo"]["name"] == "devagotchi-hub", init
        self.http("POST", "/mcp", {"jsonrpc": "2.0", "method": "notifications/initialized"})

    def http(self, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(base + path, data=data, method=method)
        req.add_header("Accept", "application/json, text/event-stream")
        if data is not None:
            req.add_header("Content-Type", "application/json")
        req.add_header("Authorization", "Bearer " + self.token)
        req.add_header("MCP-Protocol-Version", "2025-06-18")
        if self.seat:
            req.add_header("X-Devagotchi-Seat", self.seat)
        if self.session:
            req.add_header("Mcp-Session-Id", self.session)
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                return resp.status, resp.headers, resp.read()
        except urllib.error.HTTPError as error:
            return error.code, error.headers, error.read()

    def rpc(self, method, params=None):
        self.rid += 1
        body = {"jsonrpc": "2.0", "id": self.rid, "method": method}
        if params is not None:
            body["params"] = params
        status, headers, raw = self.http("POST", "/mcp", body)
        if status != 200:
            raise SystemExit(f"{self.who} {method}: HTTP {status}: {raw[:300]!r}")
        sid = headers.get("Mcp-Session-Id")
        if sid and not self.session:
            self.session = sid
        text = raw.decode()
        if headers.get("Content-Type", "").startswith("text/event-stream"):
            payload = [line[5:].strip() for line in text.splitlines() if line.startswith("data:")]
            text = payload[-1] if payload else "{}"
        message = json.loads(text)
        if "error" in message:
            raise SystemExit(f"{self.who} {method}: {message['error']}")
        return message["result"]

    def call(self, tool, arguments):
        result = self.rpc("tools/call", {"name": tool, "arguments": arguments})
        structured = result.get("structuredContent")
        if structured is None:
            structured = json.loads(result["content"][0]["text"])
        return result.get("isError", False), structured

    def ok(self, tool, arguments, label=None):
        is_error, answer = self.call(tool, arguments)
        check(label or f"{self.who} {tool}", not is_error, json.dumps(answer)[:200])
        return answer

    def refused(self, tool, arguments, code, label=None):
        is_error, answer = self.call(tool, arguments)
        good = is_error and answer.get("code") == code and bool(answer.get("next"))
        check(label or f"{self.who} {tool} refused with {code}", good, json.dumps(answer)[:200])
        return answer


P = args.project
operator = Client("operator")
claude = Client("claude")
codex = Client("codex")
reader = Client("reader")

me = operator.ok("whoami", {})
check("operator is a person on this board", me.get("human") is True and P in me.get("boards", []), json.dumps(me))
operator_subject = me.get("subject")
me = claude.ok("whoami", {})
check("claude is an agent on this board", me.get("human") is False and P in me.get("boards", []), json.dumps(me))
me = reader.ok("whoami", {})
check("reader holds no board", P not in me.get("boards", []), json.dumps(me))

prime = claude.ok("board_prime", {"project": P})
check("empty board primes empty", prime.get("held") == [] and prime.get("ready") == [] and prime.get("heartbeat_every_ms", 0) > 0,
      json.dumps(prime)[:200])

first = operator.ok("board_create", {"project": P, "title": "Write the hub deploy packet", "priority": 1,
                                     "labels": ["deploy"], "description": "config, unit, installer role"})
second = operator.ok("board_create", {"project": P, "title": "Announce the packet", "priority": 2})
deps = operator.ok("board_dep", {"project": P, "id": second["id"], "depends_on": first["id"]})
check("blocks dependency recorded", any(d["kind"] == "blocks" for d in deps), json.dumps(deps))
operator.refused("board_dep", {"project": P, "id": first["id"], "depends_on": second["id"]}, "dependency_cycle")

ready = claude.ok("board_ready", {"project": P})
check("only the unblocked ticket is ready", [t["id"] for t in ready] == [first["id"]], json.dumps([t["id"] for t in ready]))

claimed = claude.ok("board_claim", {"project": P})
lease = claimed.get("lease") or {}
check("claude claimed the ready ticket", claimed.get("claimed") is True and lease.get("ticket", {}).get("id") == first["id"]
      and lease.get("fence", 0) >= 1, json.dumps(claimed)[:200])
fence = lease.get("fence")

beat = claude.ok("board_heartbeat", {"project": P, "id": first["id"], "progress": {"step": "config written"}},
                 "claude heartbeats without restating the fence")
check("heartbeat kept the fence", beat.get("fence") == fence, json.dumps(beat)[:120])
claude.refused("board_heartbeat", {"project": P, "id": first["id"], "fence": fence + 1000}, "stale_fence",
               "a wrong fence is refused")
codex.refused("board_claim", {"project": P, "id": first["id"]}, "held_by_other")

note = claude.ok("board_note", {"project": P, "id": first["id"], "body": "the unit needs an After=network-online.target",
                                "to": operator_subject})
check("note carries the fence of the holder", note.get("fence") == fence, json.dumps(note)[:160])
found = claude.ok("board_create", {"project": P, "title": "Installer: add --role hub", "discovered_from": first["id"]})
view = claude.ok("board_get", {"project": P, "id": found["id"]})
check("discovered work links back", any(d["kind"] == "discovered-from" and d["depends_on"] == first["id"] for d in view.get("deps", [])),
      json.dumps(view.get("deps")))

prime = operator.ok("board_prime", {"project": P})
check("the note reached the operator's prime", any(n["ticket"] == first["id"] for n in prime.get("notes_to_me", [])),
      json.dumps(prime.get("notes_to_me")))

done = claude.ok("board_complete", {"project": P, "id": first["id"], "summary": "packet written; see the notes"})
check("complete moves to review", done.get("state") == "review" and done.get("holder") is None, json.dumps(done)[:160])
claude.refused("board_claim", {"project": P, "id": first["id"]}, "distinct_required", "the author may not review")
claude.refused("board_heartbeat", {"project": P, "id": first["id"]}, "stale_fence", "the finished lease is forgotten")

review = codex.ok("board_claim", {"project": P, "id": first["id"]}, "codex takes the review")
check("review lease", review.get("claimed") is True and review["lease"]["ticket"]["state"] == "reviewing", json.dumps(review)[:160])
reviewed = codex.ok("board_complete", {"project": P, "id": first["id"], "summary": "reviewed: ok"})
check("review completes to done", reviewed.get("state") == "done", json.dumps(reviewed)[:120])

ready = codex.ok("board_ready", {"project": P})
check("the blocked ticket is ready once its blocker is done", second["id"] in [t["id"] for t in ready],
      json.dumps([t["id"] for t in ready]))
held = codex.ok("board_claim", {"project": P, "id": second["id"]})
codex.refused("board_force_release", {"project": P, "id": second["id"], "reason": "mine now"}, "humans_only",
              "an agent may not force-release")
forced = operator.ok("board_force_release", {"project": P, "id": second["id"], "reason": "reassigning by hand"})
check("force-release returns it to the queue", forced.get("holder") is None and forced.get("state") == "ready",
      json.dumps(forced)[:160])
# The fence only moves with a new lease; what codex lost is the holding, so `not_holder`.
codex.refused("board_heartbeat", {"project": P, "id": second["id"]}, "not_holder", "the taken lease is refused")

current = operator.ok("board_get", {"project": P, "id": second["id"]})["ticket"]
moved = operator.ok("board_update", {"project": P, "id": second["id"], "version": current["version"], "state": "blocked",
                                     "title": "Announce the packet (after the tag)"})
check("a person moves an unheld ticket by hand", moved.get("state") == "blocked" and moved["version"] == current["version"] + 1,
      json.dumps(moved)[:160])
operator.refused("board_update", {"project": P, "id": second["id"], "version": current["version"], "priority": 0},
                 "version_conflict")
back = claude.ok("board_update", {"project": P, "id": second["id"], "version": moved["version"], "state": "ready"},
                 "an agent may move an unheld ticket along a declared transition")
check("blocked -> ready", back.get("state") == "ready", json.dumps(back)[:120])
claude.refused("board_update", {"project": P, "id": second["id"], "version": back["version"], "state": "in_progress"},
               "use_lease_ops", "active states are reached only by claiming")

reader.refused("board_prime", {"project": P}, "not_granted")
claude.refused("board_prime", {"project": "no-such-project"}, "unknown_board")

events = claude.ok("board_events", {"project": P, "after_seq": 0, "limit": 1000})
check("events chain every act", len(events) >= 12 and events[-1]["seq"] == len(events), f"{len(events)} events")
last = events[-1]["seq"]
started = time.monotonic()
waited = claude.ok("board_wait", {"project": P, "after_seq": last, "timeout_ms": 1200})
elapsed = time.monotonic() - started
check("wait returns empty after the timeout", waited == [] and 1.0 <= elapsed < 10, f"{elapsed:.1f}s")
operator.ok("board_note", {"project": P, "id": second["id"], "body": "wake the waiter"})
waited = claude.ok("board_wait", {"project": P, "after_seq": last, "timeout_ms": 5000})
check("wait returns the new event at once", len(waited) == 1 and waited[0]["action"].startswith("note"), json.dumps(waited)[:160])
chain = operator.ok("board_verify", {"project": P})
check("board chain verified", chain.get("ok") is True and chain.get("entries") == last + 1, json.dumps(chain))

# Approvals: an agent asks, waits, and never decides; people decide on /approvals.
asked = claude.ok("approval_request", {"project": P, "title": "Restart the hub", "what": "systemctl restart devagotchi-hub",
                                       "why": "new build", "undo": "start the previous build"})
check("an approval starts pending with a page to decide on",
      asked.get("status") == "pending" and asked.get("url", "").endswith(f"/approvals#{P}-{asked.get('id')}"), json.dumps(asked)[:200])
started = time.monotonic()
waited = claude.ok("approval_wait", {"project": P, "id": asked["id"], "timeout_ms": 1200})
elapsed = time.monotonic() - started
check("approval_wait holds, then says pending", waited.get("status") == "pending" and 1.0 <= elapsed < 10, f"{elapsed:.1f}s")
ticket = claude.ok("board_get", {"project": P, "id": asked["id"]})["ticket"]
claude.refused("board_update", {"project": P, "id": asked["id"], "version": ticket["version"], "state": "ready"},
               "approval_decide_only", "an agent cannot move an approval")
claude.refused("approval_wait", {"project": P, "id": first["id"], "timeout_ms": 0}, "not_an_approval")

# Account boards (15a): the subject with no grants gets a board of its own by binding.
mine = "reader-project"
reader.refused("board_prime", {"project": mine}, "unknown_board", "an unbound project is unknown, with bind as the next step")
bound = reader.ok("board_bind", {"project": mine})
check("bind makes the account's board", bound.get("created") is True and bound.get("boards") == [mine] and bound.get("limit") == 1,
      json.dumps(bound))
again = reader.ok("board_bind", {"project": mine})
check("binding again is harmless", again.get("created") is False and again.get("boards") == [mine], json.dumps(again))
me = reader.ok("whoami", {})
check("whoami lists the account board", me.get("boards") == [mine], json.dumps(me.get("boards")))
reader.refused("board_bind", {"project": "second-project"}, "plan_limit", "a free account holds one project")
reader.refused("board_bind", {"project": "Not Valid"}, "invalid")
claude.refused("board_bind", {"project": P}, "name_taken", "a shared board's name cannot be bound by one who reaches it")
owner_bound = [operator.ok("board_bind", {"project": name}) for name in ("owner-a", "owner-b")]
check("an unlimited account binds more than one", all(b.get("created") for b in owner_bound) and owner_bound[-1].get("limit") is None,
      json.dumps(owner_bound[-1]))
# Two seats of the one account: the seat is the actor on the account's own board.
seat_claude = Client("reader", seat="claude")
seat_codex = Client("reader", seat="codex")
t = reader.ok("board_create", {"project": mine, "title": "Prove seats on an account board"})
lease = seat_claude.ok("board_claim", {"project": mine, "id": t["id"]})
check("seat claude holds it as claude", lease.get("claimed") is True and lease["lease"]["ticket"].get("holder") == "claude", json.dumps(lease)[:200])
prime = seat_codex.ok("board_prime", {"project": mine})
check("seat codex sees it held by another, nothing ready", prime.get("actor") == "codex" and prime.get("ready") == [] and prime.get("held") == [],
      json.dumps({k: prime.get(k) for k in ("actor", "ready", "held")}))
seat_codex.refused("board_heartbeat", {"project": mine, "id": t["id"], "fence": lease["lease"]["fence"]}, "not_holder",
                   "the other seat cannot heartbeat a ticket it does not hold")
plain = reader.ok("board_prime", {"project": mine})
check("without a seat the account itself is the actor, and the person on its own board",
      plain.get("actor") == me.get("subject") and plain.get("human") is True, json.dumps({k: plain.get(k) for k in ("actor", "human")}))
freed = reader.ok("board_force_release", {"project": mine, "id": t["id"], "reason": "the account decides on its own board"})
check("the account may force a seat's lease free on its own board", freed.get("state") == "ready", json.dumps(freed)[:120])
listed = seat_codex.ok("board_list", {"project": mine, "holder": "@me"})
check("@me is the caller as the board names it", listed == [], json.dumps(listed)[:120])
bad_seat = Client("reader", seat="Not Valid")
is_error, answer = bad_seat.call("board_prime", {"project": mine})
check("a malformed seat is refused", is_error and answer.get("code") == "invalid", json.dumps(answer)[:160])
# On a shared board the seat is the account's own agent, named with the account.
shared_seat = Client("claude", seat="codex")
sp = shared_seat.ok("board_prime", {"project": P})
check("a seat on a shared board is <account>/<seat>", sp.get("actor") == f"{claude.ok('whoami', {})['subject']}/codex", json.dumps(sp.get("actor")))

# The command line, on the hub's host, is the other face of the same store.
result = subprocess.run([args.devagotchi, "board", "ls", "--dir", args.dir, "--json"], capture_output=True, text=True, timeout=30)
try:
    rows = json.loads(result.stdout or "[]")
    ids = {row["id"] for row in rows}
except (json.JSONDecodeError, KeyError, TypeError):
    ids = set()
check("the command line sees the tickets the tools made", result.returncode == 0 and {first["id"], second["id"], found["id"]} <= ids,
      f"exit {result.returncode}: {result.stdout.strip()[:120]!r} {result.stderr.strip()[:120]!r}")

status, _, raw = operator.http("GET", "/healthz")
health = json.loads(raw or b"{}")
audit = health.get("audit") or {}
check("hub audit chain still verifies", status == 200 and audit.get("ok") is True and audit.get("entries", 0) >= 30, json.dumps(audit))

sys.exit(1 if failures else 0)
