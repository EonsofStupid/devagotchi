//! `/` for someone not signed in: Devagotchi's product page, served by every hub. What it
//! is, how it works, how it compares with Desktop Commander, where an AI connects (this
//! hub's own `public_url`, never a hosted one) and how to run your own. A hub nobody owns
//! shares the hero and offers only the claim. A hosted hub (`tenancy.mode multi`) is never
//! claimed: its page gets people started, since their account is made the first time their
//! AI connects.
//!
//! The page is a stack of full-viewport pages under the hub's fixed top bar: each `section`
//! snaps into place as it scrolls in, and a rail beside them shows which one you are on.
//! Technical words carry tooltips (`terms.rs`); the comparison's evidence, how claiming
//! works, the AI's tools and the setup walkthrough open as popups (`compare.rs`,
//! `popups.rs`). All of it is CSS, the Popover API and inline SVG under the plain pages'
//! CSP: no script, no inline style, nothing from another origin.
//!
//! Claims are what the code does today; what is planned says "coming". The comparison cites
//! `docs/desktop-commander-teardown.md` (its source at commit c2b8795) by section, and the
//! plan's §14 where the teardown's facts are summarised there.

mod compare;
mod popups;
mod terms;

#[cfg(test)]
pub(super) use popups::TOOLS;
#[cfg(test)]
pub(super) use terms::Term;

use super::pages::Front;
use super::public::REPO;
use terms::{Term as T, term};

/// The picture of how it fits together: AI apps, the hub, the phone and machines dialing out.
const DIAGRAM: &str = include_str!("landing/diagram.svg");
const SIGN_IN: &str = "/auth/login?return_to=/account";
/// The pet from the app icon, large, beside the closing call to action.
const PET: &str = "<svg class=\"pet\" viewBox=\"0 0 200 220\" aria-hidden=\"true\" focusable=\"false\">\
<ellipse class=\"pet-egg\" cx=\"100\" cy=\"118\" rx=\"80\" ry=\"94\"/>\
<rect class=\"pet-screen\" x=\"52\" y=\"72\" width=\"96\" height=\"96\" rx=\"18\"/>\
<circle class=\"pet-face\" cx=\"80\" cy=\"110\" r=\"8\"/><circle class=\"pet-face\" cx=\"120\" cy=\"110\" r=\"8\"/>\
<path class=\"pet-smile\" d=\"M82 138q18 15 36 0\"/></svg>";
/// Opens the setup walkthrough.
const WATCH: &str = "<button type=\"button\" class=\"pop watch\" popovertarget=\"m-watch\">\
<svg class=\"play\" viewBox=\"0 0 24 24\" aria-hidden=\"true\" focusable=\"false\">\
<circle cx=\"12\" cy=\"12\" r=\"11\"/><path d=\"M9.8 7.6l6.4 4.4-6.4 4.4z\"/></svg>Watch it set up</button>";
/// Opens how claiming works.
const HOW_CLAIMING: &str =
    "<button type=\"button\" class=\"pop\" popovertarget=\"m-claim\">How claiming works</button>";
/// The full page's sections, in order, as the rail names them.
const SECTIONS: [(&str, &str); 9] = [
    ("top", "Top"),
    ("how", "How it works"),
    ("compare", "Against Desktop Commander"),
    ("features", "Effortless and secure"),
    ("connect", "Connect an AI"),
    ("self-host", "Self-host"),
    ("next", "Where it's going"),
    ("faq", "Questions"),
    ("get-started", "Get started"),
];

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
        Front::Hosted => (
            "One hub for your AI and every machine",
            full(Way::Hosted, &mcp),
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
    /// A hosted hub: an account is made the first time someone's AI connects.
    Hosted,
}

impl Way {
    fn sign_in(self) -> &'static str {
        match self {
            Way::Passkeys | Way::Hosted => "Sign in",
            Way::Shippin => "Sign in with Shippin",
        }
    }
}

/// A popup: a `popover` with a heading and a Close button that takes focus when it opens.
/// Light dismiss and Escape close it too.
fn modal(id: &str, title: &str, body: &str) -> String {
    format!(
        "<div class=\"modal\" id=\"{id}\" popover role=\"dialog\" aria-labelledby=\"{id}-title\">\
<div class=\"modal-head\"><h2 id=\"{id}-title\">{title}</h2>\
<button type=\"button\" class=\"close\" popovertarget=\"{id}\" popovertargetaction=\"hide\" \
autofocus>Close</button></div><div class=\"modal-body\">{body}</div></div>"
    )
}

/// The diagram with its ids under `prefix`, so a page can hold two copies (one shown on
/// phones, one on wide screens) without their markers and labels colliding.
fn diagram(prefix: &str) -> String {
    DIAGRAM
        .replace("id=\"dg-", &format!("id=\"{prefix}-"))
        .replace("url(#dg-", &format!("url(#{prefix}-"))
        .replace(
            "aria-labelledby=\"dg-title dg-desc\"",
            &format!("aria-labelledby=\"{prefix}-title {prefix}-desc\""),
        )
}

/// The setup video: click to play, muted, no download until then. `class` places it.
fn video(class: &str) -> String {
    format!(
        "<video class=\"{class}\" controls muted playsinline preload=\"none\" width=\"1280\" \
height=\"720\" poster=\"{poster}\" aria-label=\"Setting up Devagotchi, step by step: 57 seconds, \
captions on screen, no sound\"><source src=\"{src}\" type=\"video/mp4\"></video>",
        poster = super::media::poster_url(),
        src = super::media::video_url(),
    )
}

/// The hero: on a phone the diagram follows the words; on a wide screen the setup video
/// sits beside them.
fn hero(status: &str, action: &str) -> String {
    format!(
        "<section class=\"hero\" id=\"top\"><div class=\"hero-text\">{status}\
<h1>One hub between your AI and every machine you have.</h1>{action}</div>\
<div class=\"hero-visual\"><div class=\"diagram\">{diagram}</div>\
<figure class=\"hero-video\">{video}<figcaption>The whole setup, from an empty server to an AI \
that asks first. 57 seconds, no sound.</figcaption></figure></div></section>",
        diagram = diagram("dg"),
        video = video("turnkey"),
    )
}

/// Where you are on the page: one dot per section, the current one lit (`hub.css`).
fn rail() -> String {
    let links: String = SECTIONS
        .iter()
        .map(|(id, name)| format!("<a href=\"#{id}\"><span>{name}</span></a>"))
        .collect();
    format!("<nav class=\"rail\" aria-label=\"On this page\">{links}</nav>")
}

/// A new hub: the claim, and how to find its code. Nothing to sign in to yet.
fn unclaimed() -> String {
    let action = format!(
        "<p class=\"lead\">Nobody owns this hub yet. Claiming it makes the owner's {passkey}: \
the owner reaches every machine on it and approves every AI that connects.</p>\
<p class=\"cta\"><a class=\"button primary\" href=\"/claim\">Claim this hub</a>{WATCH}</p>\
<p class=\"meta\">Claiming takes the {code} the hub printed in its log when it started. \
{HOW_CLAIMING}</p>",
        passkey = term(T::Passkey, "passkey", 1),
        code = term(T::ClaimCode, "claim code", 1),
    );
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
newest works.</p></section>{footer}{claim}{watch}",
        hero = hero(status, &action),
        footer = footer(),
        claim = popups::claim(),
        watch = popups::watch(&video("turnkey")),
    )
}

/// A claimed hub, signed out: the whole product page.
fn full(way: Way, mcp: &str) -> String {
    let note = match way {
        Way::Passkeys => format!(
            "You sign in with a {}. Want a hub of your own? <a href=\"#self-host\">Run one with \
one command</a>.",
            term(T::Passkey, "passkey", 1)
        ),
        Way::Shippin => "A Shippin account is free. <a href=\"/docs\">Docs</a>".to_string(),
        Way::Hosted => format!(
            "No form and nothing to claim: your account is made the moment your AI connects, \
and you secure it with a {} afterwards. Have one already? <a href=\"{SIGN_IN}\">Sign in</a>.",
            term(T::Passkey, "passkey", 1)
        ),
    };
    if let Way::Hosted = way {
        let action = format!(
            "<p class=\"lead\">Devagotchi is a Desktop Commander you can own. Claude, ChatGPT, Codex \
and the rest work on your servers, PCs and devspaces through this hub. Nothing on your machines \
listens on the network, your AI asks on your phone before a step that needs your say, and every \
call is on the record.</p>\
<p class=\"cta\"><a class=\"button primary\" href=\"#connect\">Get started</a>{WATCH}</p>\
<p class=\"meta\">{note}</p>"
        );
        return [
            rail(),
            hero("", &action),
            how(way),
            compare::section(),
            features(way),
            connect(way, mcp),
            self_host(),
            next(),
            faq(),
            closing(way),
            footer(),
            popups::claim(),
            popups::tools(),
            popups::watch(&video("turnkey")),
        ]
        .concat();
    }
    let action = format!(
        "<p class=\"lead\">Devagotchi is a Desktop Commander you can own. Claude, ChatGPT, Codex \
and the rest work on your servers, PCs and devspaces through one hub, which you host yourself \
or use hosted. Nothing on your machines listens on the network, your AI asks on your phone \
before a step that needs your say, and every call is on the record.</p>\
<p class=\"cta\"><a class=\"button primary\" href=\"{SIGN_IN}\">{sign_in}</a>\
<a class=\"button\" href=\"#connect\">Connect an AI</a>{WATCH}</p><p class=\"meta\">{note}</p>",
        sign_in = way.sign_in(),
    );
    [
        rail(),
        hero("", &action),
        how(way),
        compare::section(),
        features(way),
        connect(way, mcp),
        self_host(),
        next(),
        faq(),
        closing(way),
        footer(),
        popups::claim(),
        popups::tools(),
        popups::watch(&video("turnkey")),
    ]
    .concat()
}

fn how(way: Way) -> String {
    let (get, connect) = match way {
        Way::Passkeys => (
            "Run your own with Docker Compose and claim it with your passkey, or sign in to a \
hosted one."
                .to_string(),
            format!(
                "Add this hub's address to Claude, ChatGPT or any app that takes a remote {mcp} \
server. The app signs in with {oauth} and you approve it once with your passkey.",
                mcp = term(T::Mcp, "MCP", 1),
                oauth = term(T::OAuth, "OAuth", 1),
            ),
        ),
        Way::Hosted => (
            "Use this one: it is hosted, so there is nothing to install or run. Or run your own \
with Docker Compose."
                .to_string(),
            format!(
                "Add this hub's address to Claude, ChatGPT, Codex or any app that takes a remote \
{mcp} server. The app signs in with {oauth}: press Connect once and your account is made on the \
spot, with its own space.",
                mcp = term(T::Mcp, "MCP", 1),
                oauth = term(T::OAuth, "OAuth", 1),
            ),
        ),
        Way::Shippin => (
            "Sign in to this hub with your Shippin account, or run your own with Docker Compose."
                .to_string(),
            format!(
                "Add this hub's {} address to Claude. It signs in with your Shippin account.",
                term(T::Mcp, "MCP", 1)
            ),
        ),
    };
    format!(
        "<section class=\"band\" id=\"how\" aria-labelledby=\"how-title\">\
<h2 id=\"how-title\">How it works</h2><ol class=\"steps\">\
<li><h3>Get a hub</h3><p>{get}</p></li>\
<li><h3>Add your machines</h3><p>Name a machine on your hub and paste the one line it gives \
you on that machine. It installs Devagotchi, gives the machine its own key, and starts the \
{broker} and its {link} now and after every reboot.</p></li>\
<li><h3>Connect your AI</h3><p>{connect}</p></li></ol>\
<div class=\"diagram how-diagram\">{diagram}</div></section>",
        broker = term(T::Broker, "broker", 1),
        link = term(T::Link, "link", 1),
        diagram = diagram("dgw"),
    )
}

fn features(way: Way) -> String {
    let approve = match way {
        Way::Passkeys => "You approve each one once with your passkey.",
        Way::Hosted => "You approve each one once.",
        Way::Shippin => "On this hub they sign in with your Shippin account.",
    };
    format!(
        "<section class=\"band\" id=\"features\" aria-labelledby=\"features-title\">\
<h2 id=\"features-title\">Effortless first. Security built in.</h2><div class=\"pillars\">\
<div class=\"pillar\"><h3>Effortless</h3><dl>\
<dt>One hub, every machine</dt><dd>A jump box between your AI and your servers, PCs and \
devspaces. Your AI asks the hub which machines are online and works on any of them.</dd>\
<dt>Any frontier AI</dt><dd>Claude, ChatGPT, Codex, Cursor, Gemini and others connect through \
one {mcp} address and sign in with {oauth}. {approve}</dd>\
<dt>Install in one line</dt><dd>Add a machine with one line from your hub. Paste it yourself, \
or let the AI agent in that machine's terminal run it.</dd>\
<dt>Agents share a board</dt><dd>Claude and Codex coordinate on one ticket board: claim work, \
leave notes, hand off.</dd>\
<dt>Self-host or hosted</dt><dd>A Docker image and compose with its own Postgres, claimed from \
your browser with the code from its log. Or the same hub, hosted at devagotchi.com.</dd>\
</dl></div>\
<div class=\"pillar\"><h3>Security built in</h3><dl>\
<dt>Nothing to expose</dt><dd>Each machine runs a {broker} that owns its jobs and a {link} that \
dials out to the hub. Nothing on the machine listens on the network.</dd>\
<dt>Approvals on your phone</dt><dd>Before a step that needs your say, your AI asks: what will \
change, why, and how to undo it. It arrives as a push on your phone, and the AI waits for \
your answer.</dd>\
<dt>Everything recorded</dt><dd>Every call through the hub is a row in an append-only, \
{audit}. A call that cannot be recorded is refused.</dd>\
<dt>A key per machine</dt><dd>Each machine proves its own key every time it connects. Revoke \
it and its link closes within seconds.</dd>\
</dl></div>\
<figure class=\"phone-figure\"><div class=\"phone\" aria-hidden=\"true\"><div class=\"approval\">\
<p class=\"asker\">Your AI asks</p><p class=\"ask-title\">Reload Caddy on server</p>\
<pre class=\"what\">What: systemctl reload caddy\nWhy:  new site config\nUndo: restore the backup</pre>\
<p class=\"decide\"><span class=\"yes\">Approve</span><span class=\"no\">Deny</span></p></div></div>\
<figcaption>An approval as it reaches your phone: what, why and how to undo it, then one \
tap.</figcaption></figure></div></section>",
        mcp = term(T::Mcp, "MCP", 2),
        oauth = term(T::OAuth, "OAuth", 2),
        broker = term(T::Broker, "broker", 2),
        link = term(T::Link, "link", 2),
        audit = term(T::Audit, "hash-chained audit", 2),
    )
}

fn connect(way: Way, mcp: &str) -> String {
    let (intro, clients) = match way {
        Way::Hosted => (
            format!(
                "Add this hub to any AI app that takes a remote {} server, at this address. \
Your account is made when it first connects:",
                term(T::Mcp, "MCP", 3)
            ),
            "<dt>Claude</dt><dd>On the web, desktop or phone: Customize, Connectors, Add, then \
<em>Add custom connector</em>. Name it Devagotchi, paste the address and leave the OAuth \
fields empty. Press Connect on the page that opens: that is your account made.</dd>\
<dt>ChatGPT, Codex, Cursor, Gemini and others</dt><dd>Add a remote MCP server with the same \
address. The app registers itself with this hub and signs in with OAuth; press Connect \
once.</dd>\
<dt>A terminal</dt><dd><code>devagotchi login</code> shows a code; approve it on the page it \
opens.</dd>\
<dt>Then secure it</dt><dd>Until you add a passkey on your account page, your account lives \
in that browser and the apps you connected, and nothing can recover it.</dd>",
        ),
        Way::Passkeys => (
            format!(
                "Add this hub to any AI app that takes a remote {} server, at this address:",
                term(T::Mcp, "MCP", 3)
            ),
            "<dt>Claude</dt><dd>On the web, desktop or phone: Customize, Connectors, Add, then \
<em>Add custom connector</em>. Name it Devagotchi, paste the address and leave the OAuth \
fields empty. Connect, sign in with your passkey, and approve it.</dd>\
<dt>ChatGPT, Codex, Cursor, Gemini and others</dt><dd>Add a remote MCP server with the same \
address. The app registers itself with this hub and signs in with OAuth; you approve it \
once.</dd>",
        ),
        Way::Shippin => (
            format!(
                "AI apps connect to this hub's {} address and sign in with Shippin:",
                term(T::Mcp, "MCP", 3)
            ),
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
<div><dl class=\"clients\">{clients}</dl><p class=\"meta\"><button type=\"button\" class=\"pop\" \
popovertarget=\"m-tools\">What the AI can do</button>: every tool it gets, and what each does. \
More in the <a href=\"/docs\">docs</a>.</p></div></section>"
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
<li>Claim it in your browser with the {code} from its log: <code>docker compose logs hub</code>. \
{HOW_CLAIMING}</li>\
<li>Postgres has no network at all. The hub reaches it over a Unix socket, so there is no \
database password to keep.</li>\
<li>Caddy gets the certificate for your domain.</li>\
<li>On a Debian or Ubuntu server, <code>install.sh</code> does all of this and keeps the hub \
current: it pins the image by its {digest}, deploys each new one after a backup, and keeps it \
only if the hub comes up healthy. The database is backed up nightly too.</li>\
<li>GitHub builds the image for x86_64 and ARM64 from every commit on main, with signed build \
provenance you can check.</li></ul>\
<p class=\"hosted\">Rather not run it? Use the same hub hosted at devagotchi.com, free during \
the alpha: your account there is made the first time your AI connects.</p></div></div></section>",
        code = term(T::ClaimCode, "claim code", 2),
        digest = term(T::Digest, "digest", 1),
    )
}

fn next() -> String {
    "<section class=\"band next\" id=\"next\" aria-labelledby=\"next-title\"><h2 id=\"next-title\">\
Where it's going</h2><p class=\"intro\">Coming, and not built yet:</p><ul class=\"coming\">\
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
    let (lead, href, label) = match way {
        Way::Hosted => ("Connect your AI to this hub, or run your own.", "#connect", "Get started"),
        _ => ("Sign in to this hub, or run your own.", SIGN_IN, way.sign_in()),
    };
    format!(
        "<section class=\"band closing\" id=\"get-started\" aria-labelledby=\"closing-title\">\
<div class=\"closing-card\"><div>\
<h2 id=\"closing-title\">Give your AI the whole space. Keep the keys.</h2>\
<p>{lead}</p><p class=\"cta\">\
<a class=\"button primary\" href=\"{href}\">{label}</a>\
<a class=\"button\" href=\"#self-host\">Self-host it</a></p></div>{PET}</div></section>"
    )
}

fn footer() -> String {
    format!(
        "<footer class=\"foot\"><nav aria-label=\"About this hub\"><a href=\"/docs\">Docs</a>\
<a href=\"/privacy\">Privacy</a><a href=\"/terms\">Terms</a><a href=\"/support\">Support</a>\
<a href=\"{REPO}\">Source</a></nav></footer>"
    )
}
