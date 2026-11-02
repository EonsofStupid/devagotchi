//! `/` for someone not signed in: Devagotchi's product page, served by every hub. What it
//! is, how it works, how it compares with Desktop Commander, where an AI connects (this
//! hub's own `public_url`, never a hosted one) and how to run your own. A hub nobody owns
//! shares the hero and offers only the claim.
//!
//! Claims are what the code does today; what is planned says "coming". The comparison cites
//! `docs/desktop-commander-teardown.md` (its source at commit c2b8795) by section, and the
//! plan's §14 where the teardown's facts are summarised there. Static text and inline SVG
//! under the plain pages' CSP: no script, no inline style, nothing from another origin.

use super::pages::Front;
use super::public::REPO;

/// The hero's picture: AI apps, the hub, the phone and machines dialing out.
const DIAGRAM: &str = include_str!("landing/diagram.svg");
const SIGN_IN: &str = "/auth/login?return_to=/account";
/// The pet from the app icon, large, beside the closing call to action.
const PET: &str = "<svg class=\"pet\" viewBox=\"0 0 200 220\" aria-hidden=\"true\" focusable=\"false\">\
<ellipse class=\"pet-egg\" cx=\"100\" cy=\"118\" rx=\"80\" ry=\"94\"/>\
<rect class=\"pet-screen\" x=\"52\" y=\"72\" width=\"96\" height=\"96\" rx=\"18\"/>\
<circle class=\"pet-face\" cx=\"80\" cy=\"110\" r=\"8\"/><circle class=\"pet-face\" cx=\"120\" cy=\"110\" r=\"8\"/>\
<path class=\"pet-smile\" d=\"M82 138q18 15 36 0\"/></svg>";

/// The title and body of `/` for this kind of hub.
pub fn page(front: &Front, public_url: &str) -> (&'static str, String) {
    let mcp = super::pages::escape(&format!("{}/mcp", public_url.trim_end_matches('/')));
    match front {
        Front::Unclaimed => ("A new Devagotchi hub", unclaimed()),
        Front::Passkeys => (
            "One hub for your AI and every machine",
            full(Way::Passkeys, &mcp),
        ),
        Front::Shippin => (
            "One hub for your AI and every machine",
            full(Way::Shippin, &mcp),
        ),
    }
}

/// How people and AI apps sign in to this hub.
#[derive(Clone, Copy)]
enum Way {
    /// The hub's own accounts: passkeys, and OAuth for AI apps.
    Passkeys,
    /// Sign in with Shippin (Nomen).
    Shippin,
}

impl Way {
    fn sign_in(self) -> &'static str {
        match self {
            Way::Passkeys => "Sign in",
            Way::Shippin => "Sign in with Shippin",
        }
    }
}

fn hero(status: &str, action: &str) -> String {
    format!(
        "<section class=\"hero\"><div class=\"hero-text\">{status}\
<h1>One hub between your AI and every machine you have.</h1>{action}</div>\
<div class=\"diagram\">{DIAGRAM}</div></section>"
    )
}

/// A new hub: the claim, and how to find its code. Nothing to sign in to yet.
fn unclaimed() -> String {
    let action = "<p class=\"lead\">Nobody owns this hub yet. Claiming it makes the owner's \
passkey: the owner reaches every machine on it and approves every AI that connects.</p>\
<p class=\"cta\"><a class=\"button primary\" href=\"/claim\">Claim this hub</a></p>\
<p class=\"meta\">Claiming takes the code the hub printed in its log when it started. \
<a href=\"/docs\">How it works</a></p>";
    let status = "<p class=\"state\"><span class=\"dot\"></span>Waiting for its owner</p>";
    format!(
        "{hero}<section class=\"band\" aria-labelledby=\"claim-steps\">\
<h2 id=\"claim-steps\">Claim it in a minute</h2><ol class=\"steps\">\
<li><h3>Find the link in the log</h3><p>The hub printed a claim link when it started. With \
Docker Compose:</p><pre class=\"terminal\">docker compose logs hub | grep -A1 &quot;Claim it in a browser&quot;</pre></li>\
<li><h3>Open it where your passkey lives</h3><p>Open the link on the phone or computer that \
will hold the owner's passkey, or open the claim page and type the code.</p></li>\
<li><h3>Make your passkey</h3><p>Give the owner a name and make the passkey. You are signed \
in, and the code stops working for good.</p></li></ol>\
<p class=\"meta\">The code works for a day. Restarting the hub prints a new one, and only the \
newest works.</p></section>{footer}",
        hero = hero(status, action),
        footer = footer()
    )
}

/// A claimed hub, signed out: the whole product page.
fn full(way: Way, mcp: &str) -> String {
    let note = match way {
        Way::Passkeys => {
            "You sign in with a passkey. Want a hub of your own? <a href=\"#self-host\">Run one with one command</a>."
        }
        Way::Shippin => "A Shippin account is free. <a href=\"/docs\">Docs</a>",
    };
    let action = format!(
        "<p class=\"lead\">Devagotchi is a Desktop Commander you can own. Claude, ChatGPT, Codex \
and the rest work on your servers, PCs and devspaces through one hub, which you host yourself \
or use hosted. Nothing on your machines listens on the network, your AI asks on your phone \
before a step that needs your say, and every call is on the record.</p>\
<p class=\"cta\"><a class=\"button primary\" href=\"{SIGN_IN}\">{sign_in}</a>\
<a class=\"button\" href=\"#connect\">Connect an AI</a></p><p class=\"meta\">{note}</p>",
        sign_in = way.sign_in(),
    );
    [
        hero("", &action),
        how(way),
        compare(),
        features(way),
        connect(way, mcp),
        self_host(),
        next(),
        faq(),
        closing(way),
        footer(),
    ]
    .concat()
}

fn how(way: Way) -> String {
    let (get, connect) = match way {
        Way::Passkeys => (
            "Run your own with Docker Compose and claim it with your passkey, or sign in to a \
hosted one.",
            "Add this hub's address to Claude, ChatGPT or any app that takes a remote MCP \
server. The app signs in with OAuth and you approve it once with your passkey.",
        ),
        Way::Shippin => (
            "Sign in to this hub with your Shippin account, or run your own with Docker Compose.",
            "Add this hub's address to Claude. It signs in with your Shippin account.",
        ),
    };
    format!(
        "<section class=\"band\" id=\"how\" aria-labelledby=\"how-title\">\
<h2 id=\"how-title\">How it works</h2><ol class=\"steps\">\
<li><h3>Get a hub</h3><p>{get}</p></li>\
<li><h3>Add your machines</h3><p>Name a machine on your hub and paste the one line it gives \
you on that machine. It installs Devagotchi, gives the machine its own key, and starts the \
broker and its link now and after every reboot.</p></li>\
<li><h3>Connect your AI</h3><p>{connect}</p></li></ol></section>"
    )
}

/// Desktop Commander against Devagotchi, row by row. Each Desktop Commander fact carries
/// where it is shown: a section of the teardown, or the plan's §14.
const ROWS: [(&str, &str, &str, &str); 11] = [
    (
        "Who runs the remote side",
        "Desktop Commander's own service. Its server is not in the open repository.",
        "§3.11",
        "You do, with one Docker command. Or use the hosted hub.",
    ),
    (
        "Your calls on the way",
        "Each call's arguments and its result sit in a row of their cloud database (Supabase) until it is swept.",
        "§3.1, §3.7",
        "From your hub straight down the machine's own link. The hub stores neither arguments nor output.",
    ),
    (
        "A machine's credential",
        "Tokens in a file, device.json. A copy of the file is the device.",
        "§3.4, plan §14",
        "Its own Ed25519 key, proved over a fresh challenge every time it connects.",
    ),
    (
        "Before a risky step",
        "No approval step.",
        "plan §14",
        "Your AI asks, and you approve or deny on your phone.",
    ),
    (
        "What a command may run",
        "Anything but 34 blocked names, matched on each command's first word. The AI can change the list itself.",
        "§1.4, §1.11",
        "Each machine decides: only the programs you granted, or what its own account may. On Linux that is a service account, never root.",
    ),
    (
        "The record",
        "Log files on the machine with every call's full arguments, rotated at 10 MB.",
        "§1.11",
        "An append-only, hash-chained audit on the hub. A call it cannot record is refused.",
    ),
    (
        "Long jobs",
        "Processes tracked by OS PID. A single wait is capped at 50 seconds.",
        "§1.5",
        "The machine's broker owns each job. It outlives the AI's connection; disconnecting never stops it.",
    ),
    (
        "Telemetry",
        "On by default. Install and setup send events without checking the opt-out.",
        "§1.4, §2.1",
        "None.",
    ),
    (
        "Several agents at once",
        "No shared board among its 25 tools.",
        "§1.3",
        "A ticket board the agents share: claim, notes, handoff.",
    ),
    (
        "What it runs on",
        "Node.js 22.12 or later.",
        "§1.1",
        "One Rust binary.",
    ),
    (
        "Price",
        "Free up to 10,000 remote tool calls a month, then $20 a month.",
        "§6.2",
        "Self-hosted: no limit on calls. Hosted: free during the alpha.",
    ),
];

fn compare() -> String {
    let rows: String = ROWS
        .iter()
        .map(|(what, dc, cite, dg)| {
            format!(
                "<tr><th scope=\"row\">{what}</th><td data-label=\"Desktop Commander\">{dc} \
<span class=\"cite\">{cite}</span></td><td class=\"ours\" data-label=\"Devagotchi\">{dg}</td></tr>"
            )
        })
        .collect();
    format!(
        "<section class=\"band\" id=\"compare\" aria-labelledby=\"compare-title\">\
<h2 id=\"compare-title\">Desktop Commander is #1. Devagotchi gives your AI the whole secure \
space.</h2>\
<p class=\"intro\">Desktop Commander puts a computer in the chat. Devagotchi is built for \
everything around that: every machine you have, a hub you own, and you in the loop. Side by \
side, from Desktop Commander's own source:</p>\
<div class=\"compare-wrap\"><table class=\"compare\"><thead><tr><th scope=\"col\"><span \
class=\"visually-hidden\">What</span></th><th scope=\"col\">Desktop Commander</th>\
<th scope=\"col\" class=\"ours\">Devagotchi</th></tr></thead><tbody>{rows}</tbody></table></div>\
<p class=\"source\">Desktop Commander is ahead today on macOS, which Devagotchi does not run \
on yet, and on PDF, Excel and Word files (§1.9). Its facts here are read from its source at \
commit c2b8795 (1 November 2026); § marks a section of \
<a href=\"{REPO}/blob/main/docs/desktop-commander-teardown.md\">our teardown</a>, where every \
claim is cited to a file and line, and plan §14 the comparison in \
<a href=\"{REPO}/blob/main/docs/product-and-platform-plan.md\">our plan</a>.</p></section>"
    )
}

fn features(way: Way) -> String {
    let approve = match way {
        Way::Passkeys => "You approve each one once with your passkey.",
        Way::Shippin => "On this hub they sign in with your Shippin account.",
    };
    format!(
        "<section class=\"band\" id=\"features\" aria-labelledby=\"features-title\">\
<h2 id=\"features-title\">Effortless first. Security built in.</h2><div class=\"pillars\">\
<div class=\"pillar\"><h3>Effortless</h3><dl>\
<dt>One hub, every machine</dt><dd>A jump box between your AI and your servers, PCs and \
devspaces. Your AI asks the hub which machines are online and works on any of them.</dd>\
<dt>Any frontier AI</dt><dd>Claude, ChatGPT, Codex, Cursor, Gemini and others connect through \
one MCP address and sign in with OAuth. {approve}</dd>\
<dt>Install in one line</dt><dd>Add a machine with one line from your hub. Paste it yourself, \
or let the AI agent in that machine's terminal run it.</dd>\
<dt>Agents share a board</dt><dd>Claude and Codex coordinate on one ticket board: claim work, \
leave notes, hand off.</dd>\
<dt>Self-host or hosted</dt><dd>A Docker image and compose with its own Postgres, claimed from \
your browser with the code from its log. Or the same hub, hosted at devagotchi.com.</dd>\
</dl></div>\
<div class=\"pillar\"><h3>Security built in</h3><dl>\
<dt>Nothing to expose</dt><dd>Each machine runs a broker that owns its jobs and a link that \
dials out to the hub. Nothing on the machine listens on the network.</dd>\
<dt>Approvals on your phone</dt><dd>Before a step that needs your say, your AI asks: what will \
change, why, and how to undo it. It arrives as a push on your phone, and the AI waits for \
your answer.</dd>\
<dt>Everything recorded</dt><dd>Every call through the hub is a row in an append-only, \
hash-chained audit. A call that cannot be recorded is refused.</dd>\
<dt>A key per machine</dt><dd>Each machine proves its own key every time it connects. Revoke \
it and its link closes within seconds.</dd>\
</dl></div>\
<figure class=\"phone-figure\"><div class=\"phone\" aria-hidden=\"true\"><div class=\"approval\">\
<p class=\"asker\">Your AI asks</p><p class=\"ask-title\">Reload Caddy on server</p>\
<pre class=\"what\">What: systemctl reload caddy\nWhy:  new site config\nUndo: restore the backup</pre>\
<p class=\"decide\"><span class=\"yes\">Approve</span><span class=\"no\">Deny</span></p></div></div>\
<figcaption>An approval as it reaches your phone: what, why and how to undo it, then one \
tap.</figcaption></figure></div></section>"
    )
}

fn connect(way: Way, mcp: &str) -> String {
    let (intro, clients) = match way {
        Way::Passkeys => (
            "Add this hub to any AI app that takes a remote MCP server, at this address:",
            "<dt>Claude</dt><dd>On the web, desktop or phone: Customize, Connectors, Add, then \
<em>Add custom connector</em>. Name it Devagotchi, paste the address and leave the OAuth \
fields empty. Connect, sign in with your passkey, and approve it.</dd>\
<dt>ChatGPT, Codex, Cursor, Gemini and others</dt><dd>Add a remote MCP server with the same \
address. The app registers itself with this hub and signs in with OAuth; you approve it \
once.</dd>",
        ),
        Way::Shippin => (
            "AI apps connect to this hub at this address and sign in with Shippin:",
            "<dt>Claude</dt><dd>Customize, Connectors, Add, then <em>Add custom connector</em> \
with the address above. Choose <em>Use your own OAuth client</em> and enter the client ID \
this hub's operator gives you, then sign in with your Shippin account.</dd>\
<dt>Other AI apps</dt><dd>Their sign-in on this hub is not finished yet; the docs will say \
when it is.</dd>",
        ),
    };
    format!(
        "<section class=\"band connect\" id=\"connect\" aria-labelledby=\"connect-title\">\
<div><h2 id=\"connect-title\">Connect an AI</h2><p>{intro}</p>\
<p class=\"address\"><code>{mcp}</code></p></div>\
<div><dl class=\"clients\">{clients}</dl><p class=\"meta\">Every tool your AI gets, and what it \
does: <a href=\"/docs\">Docs</a>.</p></div></section>"
    )
}

fn self_host() -> String {
    format!(
        "<section class=\"band self-host\" id=\"self-host\" aria-labelledby=\"self-host-title\">\
<h2 id=\"self-host-title\">Self-host with one command</h2>\
<p class=\"intro\">On any Linux box with Docker: name your hub once, then one command brings up \
the hub, its own Postgres and Caddy for TLS.</p>\
<div class=\"split\"><pre class=\"terminal\"><span class=\"c\"># once: get it and name your hub</span>\n\
git clone {REPO}\ncd devagotchi/deploy/compose\ncp .env.example .env   \
<span class=\"c\"># set DEVAGOTCHI_DOMAIN</span>\n\n<span class=\"c\"># the one command</span>\n\
docker compose up -d</pre>\
<div><ul class=\"facts\">\
<li>Claim it in your browser with the code from its log: <code>docker compose logs hub</code>.</li>\
<li>Postgres has no network at all. The hub reaches it over a Unix socket, so there is no \
database password to keep.</li>\
<li>Caddy gets the certificate for your domain.</li>\
<li>GitHub builds the image for x86_64 and ARM64 from every commit on main, with signed build \
provenance you can check.</li></ul>\
<p class=\"hosted\">Rather not run it? Use the same hub hosted at devagotchi.com, free during \
the alpha. New accounts there open with invitations, which are coming.</p></div></div></section>"
    )
}

fn next() -> String {
    "<section class=\"band next\" aria-labelledby=\"next-title\"><h2 id=\"next-title\">Where \
it's going</h2><p class=\"intro\">Coming, and not built yet:</p><ul class=\"coming\">\
<li><strong>Devagotchi's own private mesh.</strong> The link moves onto Iroh: your machines \
connect directly when they can, through a relay when they can't, and fall back to today's link \
whenever that fails.</li>\
<li><strong>Secrets and devspaces</strong>, from the same hub.</li></ul></section>"
        .to_string()
}

const FAQ: [(&str, &str); 8] = [
    (
        "Is Devagotchi a fork of Desktop Commander?",
        "No. It is written from scratch in Rust to replace it: a broker on each machine that owns its jobs, a thin MCP adapter, and the hub between them and your AI.",
    ),
    (
        "Does anything on my machine accept connections?",
        "No. The machine's link opens one outbound WebSocket to the hub and keeps it. No port opens on the machine, so it works behind any router or firewall.",
    ),
    (
        "What can my AI do on a machine?",
        "Start programs as jobs and read their output; read, write and edit files; list folders; read images; and ask you to approve a step. Each machine decides what may run: only the programs you granted, or anything its own account can.",
    ),
    (
        "What does the hub keep?",
        "One row per call: the time, the account, the tool, the machine, the job and the outcome, each row chained to the one before by hash. Not the program's arguments and not its output: output passes through to your AI app, and the broker keeps it on your machine.",
    ),
    (
        "Which systems can join a hub?",
        "Linux on x86_64 and ARM64, and Windows. macOS is coming.",
    ),
    (
        "Does it send telemetry?",
        "No. There are no analytics or tracking scripts, and the broker and the MCP adapter make no network calls at all: the build refuses HTTP client libraries in them.",
    ),
    (
        "Which AI apps work?",
        "Any app that connects to a remote MCP server with OAuth: Claude on the web, desktop and phone, ChatGPT, Codex, Cursor, Gemini and others. They all use the same address.",
    ),
    (
        "What does it cost?",
        "Self-hosting runs on your own hardware, with no limit on calls. The hosted hub is free during its alpha.",
    ),
];

fn faq() -> String {
    let items: String = FAQ
        .iter()
        .map(|(q, a)| format!("<details><summary>{q}</summary><p>{a}</p></details>"))
        .collect();
    format!(
        "<section class=\"band faq\" id=\"faq\" aria-labelledby=\"faq-title\">\
<h2 id=\"faq-title\">Questions</h2>{items}<p class=\"meta\">The source, and the Desktop \
Commander teardown this page cites, are on GitHub: \
<a href=\"{REPO}\">EonsofStupid/devagotchi</a>.</p></section>"
    )
}

fn closing(way: Way) -> String {
    format!(
        "<section class=\"band closing\" aria-labelledby=\"closing-title\"><div>\
<h2 id=\"closing-title\">Give your AI the whole space. Keep the keys.</h2>\
<p>Sign in to this hub, or run your own.</p><p class=\"cta\">\
<a class=\"button primary\" href=\"{SIGN_IN}\">{sign_in}</a>\
<a class=\"button\" href=\"#self-host\">Self-host it</a></p></div>{PET}</section>",
        sign_in = way.sign_in()
    )
}

fn footer() -> String {
    format!(
        "<footer class=\"foot\"><nav aria-label=\"About this hub\"><a href=\"/docs\">Docs</a>\
<a href=\"/privacy\">Privacy</a><a href=\"/terms\">Terms</a><a href=\"/support\">Support</a>\
<a href=\"{REPO}\">Source</a></nav></footer>"
    )
}
