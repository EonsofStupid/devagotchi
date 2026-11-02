//! The hub's public pages: what Devagotchi is and how to connect an AI (`/docs`), the
//! privacy policy, the terms and where to get help. They are what the AI directories link to
//! (Claude's Connectors Directory, OpenAI's plugin directory), so they describe this hub as
//! it runs, never planned behavior. Static text, no sign-in, no script.

use axum::{extract::State, http::StatusCode, response::Response};

use super::pages;
use crate::AppState;

/// The date every page below was last changed. Change it with the text.
const UPDATED: &str = "1 November 2026";
const OPERATOR: &str = "AngryVibes LLC";
pub(super) const REPO: &str = "https://github.com/EonsofStupid/devagotchi";

fn page(title: &str, body: &str) -> Response {
    pages::html(
        StatusCode::OK,
        title,
        &format!(
            "{body}<p class=\"meta\">Last updated {UPDATED}. \
<a href=\"/docs\">Docs</a> · <a href=\"/privacy\">Privacy</a> · <a href=\"/terms\">Terms</a> · \
<a href=\"/support\">Support</a></p>"
        ),
    )
}

/// `/docs`: what Devagotchi is and how to connect an AI to this hub, at this hub's own
/// address and with the sign-in it runs.
pub async fn docs(State(state): State<AppState>) -> Response {
    let mcp = pages::escape(&format!(
        "{}/mcp",
        state.hub.config.public_url.trim_end_matches('/')
    ));
    let connect = if state.passkeys.is_some() {
        format!(
            "<p>In Claude (web, desktop or mobile): Customize, Connectors, Add, <em>Add custom connector</em>. \
Name it Devagotchi and use the URL <code>{mcp}</code>; leave the OAuth fields empty. Claude \
registers itself with this hub: connect, sign in with your passkey, and approve it.</p>\
<p>Any other AI app that takes a remote MCP server with OAuth connects the same way, at the \
same URL. You see and end every connection from your account.</p>"
        )
    } else if matches!(
        state.hub.config.auth,
        crate::config::AuthConfig::Nomen { .. }
    ) {
        format!(
            "<p>In Claude (web, desktop or mobile): Customize, Connectors, Add, <em>Add custom connector</em>. \
Name it Devagotchi and use the URL <code>{mcp}</code>. On the next step \
choose <em>Use your own OAuth client</em> and enter the client ID of this hub's AI app in \
Shippin: its operator has it (<code>devagotchi hub connect-nomen</code> printed it). \
Connect, and sign in with your Shippin account.</p>\
<p>Other AI apps need sign-in support that is not finished yet; this page will say when it is.</p>"
        )
    } else {
        format!(
            "<p>Add a custom connector in Claude with the URL <code>{mcp}</code>, and sign in the \
way this hub's operator set up.</p>"
        )
    };
    page(
        "Docs",
        &format!(
            "<h1>Devagotchi</h1>\
<p>Devagotchi lets an AI work on your own machines (servers, PCs and devspaces) through one hub \
you sign in to. Each machine runs a broker that owns its jobs and a link that dials out to the \
hub; nothing on the machine listens on the network, and the services run as a dedicated \
account, never as root.</p>\
<h2>Connect Claude</h2>{connect}\
<h2>Add a machine</h2>\
<p>A machine joins a hub with a one-time code from the hub's operator. The Linux installer \
(x86_64 and ARM64, systemd) and the Windows installer are in the \
<a href=\"{REPO}\">repository</a>, under <code>deploy/</code>.</p>\
<h2>Tools</h2>\
<table><thead><tr><th>Tool</th><th>What it does</th></tr></thead><tbody>\
<tr><td><code>machines</code></td><td>The machines you can use and whether each is online now. Start here.</td></tr>\
<tr><td><code>operation_start</code></td><td>Runs a program the machine's owner has allowed, with exact arguments, as a job the machine's broker owns.</td></tr>\
<tr><td><code>operation_get</code>, <code>operation_read</code></td><td>A job's state, and its output after a cursor.</td></tr>\
<tr><td><code>operation_cancel</code></td><td>Stops a job. Disconnecting never does.</td></tr>\
<tr><td><code>approval_request</code>, <code>approval_wait</code></td><td>Asks you to approve a step, with what, why and how to undo it; you decide on <a href=\"/approvals\">Approvals</a> and the AI continues on your answer.</td></tr>\
<tr><td><code>whoami</code></td><td>The account you are signed in as and what it may use.</td></tr>\
<tr><td><code>board_*</code></td><td>The agents' ticket board for a project: claim work, leave notes, hand off, complete.</td></tr>\
</tbody></table>"
        ),
    )
}

pub async fn privacy() -> Response {
    page(
        "Privacy policy",
        &format!(
            "<h1>Privacy policy</h1>\
<p>Devagotchi at app.devagotchi.com and devagotchi.com is run by {OPERATOR} (\"we\"). This page \
says what the service keeps about you, why, and for how long.</p>\
<h2>What we keep</h2>\
<ul>\
<li><strong>Your account.</strong> You sign in through Shippin (id.shippin.cloud). The hub receives \
your account ID and login name from it. So that your phone stays signed in, the hub keeps your sign-in \
session on its own disk, with the tokens from Shippin encrypted. The session ends when you sign out, \
after 30 days unused, after 90 days at most, or when Shippin stops accepting it.</li>\
<li><strong>A record of every tool call.</strong> The time, your account ID, how you signed in, \
the tool, the machine, the job's ID and the outcome. It does not record the program's arguments \
or its output.</li>\
<li><strong>Your machines.</strong> Each enrolled machine's name, the public key it signs in with, \
and when it was enrolled or revoked.</li>\
<li><strong>Boards.</strong> The tickets and notes you and your agents write. A project's board \
belongs to your organization on Shippin: the other members of that organization, and their agents, \
see and work it. Nobody outside it does.</li>\
</ul>\
<p>The output of a job passes through the hub to your AI app and is not stored by the hub. It is \
kept by the broker on your own machine, under your control.</p>\
<h2>What we do not do</h2>\
<p>No advertising, no selling or renting of data, no analytics or tracking scripts on these \
pages, and no use of your data to train models.</p>\
<h2>Who else sees it</h2>\
<p>The servers are ours, on hardware we rent. Your AI app (for example Claude) receives the tool \
results you ask for, under its own privacy policy. We disclose data to anyone else only when the \
law requires it.</p>\
<h2>How long we keep it</h2>\
<p>The record of tool calls is append-only and kept as a security record while the service runs. \
Machine records and boards are kept until you or your hub's operator delete them. Ask us to delete \
your data and we remove everything that is not needed for that security record.</p>\
<h2>Contact</h2>\
<p>Open an issue at <a href=\"{REPO}/issues\">{REPO}/issues</a>, or see <a href=\"/support\">Support</a>.</p>"
        ),
    )
}

pub async fn terms() -> Response {
    page(
        "Terms of service",
        &format!(
            "<h1>Terms of service</h1>\
<p>Devagotchi is provided by {OPERATOR}. By connecting an AI app or a machine to it, you agree \
to these terms.</p>\
<ul>\
<li><strong>Alpha.</strong> The service is free while it is in alpha, and it may change or stop. \
We post changes to these terms on this page with a new date.</li>\
<li><strong>Your machines, your responsibility.</strong> Connect only machines you own or are \
allowed to manage. You are responsible for what you and the AI apps you connect run through \
Devagotchi, and for the programs you allow each machine to run.</li>\
<li><strong>Acceptable use.</strong> Do not use Devagotchi to break the law, to reach machines \
without permission, or to attack or overload any system, including ours.</li>\
<li><strong>No warranty.</strong> The service is provided as is, without warranties of any kind, \
to the extent the law allows.</li>\
<li><strong>Limited liability.</strong> To the extent the law allows, {OPERATOR} is not liable for \
indirect or consequential damages, or for loss of data on machines you connect.</li>\
<li><strong>Ending.</strong> You can disconnect at any time by removing the connector and revoking \
your machines. We may suspend access that breaks these terms.</li>\
</ul>\
<p>Questions: see <a href=\"/support\">Support</a>.</p>"
        ),
    )
}

pub async fn support() -> Response {
    page(
        "Support",
        &format!(
            "<h1>Support</h1>\
<p>For help, bug reports and data requests, open an issue at \
<a href=\"{REPO}/issues\">{REPO}/issues</a>. Say which machine and which AI app, and what you \
expected to happen.</p>\
<p>To report a security problem privately, use <a href=\"{REPO}/security/advisories/new\">GitHub's \
private vulnerability report</a> instead of a public issue.</p>\
<p>Setup and tools: <a href=\"/docs\">Docs</a>.</p>"
        ),
    )
}
