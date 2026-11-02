//! Desktop Commander against Devagotchi, row by row, and each row's evidence in a popup.
//! Every Desktop Commander fact carries where it is shown: a section of
//! `docs/desktop-commander-teardown.md` (its source at commit c2b8795, every claim cited to a
//! file and line there), or the plan's §14 where the teardown's facts are summarised. The
//! evidence quotes those citations; what the teardown marks INFERRED says so here too.

use super::super::public::REPO;
use super::modal;
use super::terms::{Term, term};

/// One comparison row.
pub struct Row {
    pub what: &'static str,
    pub dc: &'static str,
    pub cite: &'static str,
    pub ours: &'static str,
    /// What the cited section shows, with its file and line citations.
    pub evidence: &'static str,
    /// The heading anchor of the first cited section: `t:` in the teardown, `p:` in the plan.
    pub anchor: &'static str,
}

pub const ROWS: [Row; 11] = [
    Row {
        what: "Who runs the remote side",
        dc: "Desktop Commander's own service. Its server is not in the open repository.",
        cite: "§3.11",
        ours: "You do, with one Docker command. Or use the hosted hub.",
        evidence: "<p>Remote MCP goes through Desktop Commander's hosted service, \
<code>mcp.desktopcommander.app</code> by default (<code>src/remote-device/device.ts:104</code>).</p>\
<p>Not in the repository at that commit: the Remote MCP server that ChatGPT and Claude connect \
to and its OAuth, the device sign-in endpoints, the dashboard, and the database schema and \
policies. The teardown searched <code>src/</code>, <code>scripts/</code> and <code>test/</code> for them.</p>",
        anchor: "t:311-what-is-not-in-the-repo-cloud-side",
    },
    Row {
        what: "Your calls on the way",
        dc: "Each call's arguments and its result sit in a row of their cloud database (Supabase) until it is swept.",
        cite: "§3.1, §3.7",
        ours: "From your hub straight down the machine's own link. The hub stores neither arguments nor output.",
        evidence: "<p>A call is a row in the <code>mcp_remote_calls</code> table of a Supabase \
Postgres database. The device claims the row, runs the call, and writes the result back into \
the same row (<code>src/remote-device/remote-channel.ts:912-1004</code>, \
<code>:1269-1321</code>).</p><p>Its README says the service stores tool arguments and results \
there temporarily, and sweeps finished rows at least a minute after they complete \
(<code>src/remote-device/README.md:220</code>). The service's side of this is not in the \
repository, so the teardown marks those steps INFERRED.</p>",
        anchor: "t:31-architecture-as-implemented",
    },
    Row {
        what: "A machine's credential",
        dc: "Tokens in a file, device.json. A copy of the file is the device.",
        cite: "§3.4, plan §14",
        ours: "Its own Ed25519 key, proved over a fresh challenge every time it connects.",
        evidence: "<p><code>desktop-commander remote</code> keeps its access and refresh tokens in \
<code>~/.desktop-commander-device/device.json</code>, written with mode 0600 \
(<code>src/remote-device/device.ts:40-59</code>, <code>:466-503</code>). Keeping the session \
is on by default (<code>src/remote-device/device.ts:109-113</code>).</p><p>The plan's §14 sums \
it up: there is no device key, so a copied <code>device.json</code> is the whole account.</p>",
        anchor: "t:34-token-storage-persistence-and-refresh",
    },
    Row {
        what: "Before a risky step",
        dc: "No approval step.",
        cite: "plan §14",
        ours: "Your AI asks, and you approve or deny on your phone.",
        evidence: "<p>The plan's §14, from the teardown: no approval before a command. A call the \
device claims from its queue goes to the local server and runs \
(<code>src/remote-device/device.ts:600-694</code>, teardown §3.7).</p>",
        anchor: "p:14-built-in-sign-in-and-pairing-d25-ruled-2026-10-27",
    },
    Row {
        what: "What a command may run",
        dc: "Anything but 34 blocked names, matched on each command's first word. The AI can change the list itself.",
        cite: "§1.4, §1.11",
        ours: "Each machine decides: only the programs you granted, or what its own account may. On Linux that is a service account, never root.",
        evidence: "<p>The default <code>blockedCommands</code> list has 34 names, from \
<code>mkfs</code> and <code>sudo</code> to <code>shutdown</code> \
(<code>src/config-manager.ts:233-277</code>). The check splits a command line on \
<code>;</code>, <code>&amp;&amp;</code>, <code>||</code>, <code>|</code> and <code>&amp;</code> \
and matches the first word of each part (<code>src/command-manager.ts:16-267</code>).</p>\
<p>The model can rewrite the list with <code>set_config_value</code>; the only guard is a \
note in that tool's description (<code>src/server.ts:339-365</code>). From reading the parser \
(INFERRED, not run): a newline is not a separator, and <code>bash -c \"…\"</code> hides what it \
runs.</p>",
        anchor: "t:14-configuration-file-and-every-key",
    },
    Row {
        what: "The record",
        dc: "Log files on the machine with every call's full arguments, rotated at 10 MB.",
        cite: "§1.11",
        ours: "An append-only, hash-chained audit on the hub. A call it cannot record is refused.",
        evidence: "<p>Every call's arguments go unredacted to <code>claude_tool_call.log</code>, \
rotated at 10 MB with the rotated files kept, and calls with their results to \
<code>tool-history.jsonl</code>, capped at 1000 records (<code>src/config.ts:10-11</code>, \
<code>src/utils/toolHistory.ts:55-74</code>). The teardown files them as local audit trails, \
not boundaries.</p>",
        anchor: "t:111-security-boundaries",
    },
    Row {
        what: "Long jobs",
        dc: "Processes tracked by OS PID. A single wait is capped at 50 seconds.",
        cite: "§1.5",
        ours: "The machine's broker owns each job. It outlives the AI's connection; disconnecting never stops it.",
        evidence: "<p>Terminal sessions live in a map keyed by OS PID \
(<code>src/terminal-manager.ts:114-144</code>). Any single blocking wait is capped at \
<code>MAX_PROCESS_WAIT_MS = 50000</code>, to stay under the MCP SDK's 60-second request \
timeout (<code>src/config.ts:13-23</code>).</p>",
        anchor: "t:15-processes-and-terminal-sessions",
    },
    Row {
        what: "Telemetry",
        dc: "On by default. Install and setup send events without checking the opt-out.",
        cite: "§1.4, §2.1",
        ours: "None.",
        evidence: "<p><code>telemetryEnabled</code> defaults to <code>true</code> \
(<code>src/config-manager.ts:280</code>). The npm <code>postinstall</code> script sends a \
<code>package_installed</code> event, and the <code>setup</code> command about thirty \
<code>npx_setup_*</code> events, neither checking the opt-out \
(<code>track-installation.js:43-78</code>, <code>setup-claude-server.js:14-27</code>).</p>",
        anchor: "t:14-configuration-file-and-every-key",
    },
    Row {
        what: "Several agents at once",
        dc: "No shared board among its 25 tools.",
        cite: "§1.3",
        ours: "A ticket board the agents share: claim, notes, handoff.",
        evidence: "<p><code>tools/list</code> advertises 25 tools \
(<code>src/server.ts:306-1158</code>): configuration, files, search, processes, PDFs and \
feedback. None of them is a board or queue that several agents share.</p>",
        anchor: "t:13-complete-tool-list-and-parameters",
    },
    Row {
        what: "What it runs on",
        dc: "Node.js 22.12 or later.",
        cite: "§1.1",
        ours: "One Rust binary.",
        evidence: "<p><code>package.json</code> requires Node.js 22.12.0 or later, and its \
<code>install.sh</code> enforces the same (<code>package.json:15-17</code>, \
<code>install.sh:22-24</code>).</p>",
        anchor: "t:11-runtime-language-entry-points",
    },
    Row {
        what: "Price",
        dc: "Free up to 10,000 remote tool calls a month, then $20 a month.",
        cite: "§6.2",
        ours: "Self-hosted: no limit on calls. Hosted: free during the alpha.",
        evidence: "<p>Its pricing page, read on 1 November 2026: Free, $0 a month with 10,000 \
tool calls a month; Pro, $20 a month with unlimited tool calls. The local server is free and \
open source. Nothing in the repository counts calls, so the teardown infers the cap is \
metered on Remote MCP calls by the hosted service.</p>",
        anchor: "t:62-pricing--plans",
    },
];

/// The table, and one evidence popup per row (opened from the row's citation).
pub fn section() -> String {
    let rows: String = ROWS
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let ours = if row.what == "The record" {
                row.ours.replace(
                    "hash-chained audit",
                    &term(Term::Audit, "hash-chained audit", 1),
                )
            } else {
                row.ours.to_string()
            };
            format!(
                "<tr><th scope=\"row\">{what}</th><td data-label=\"Desktop Commander\">{dc} \
<button type=\"button\" class=\"evidence\" popovertarget=\"ev-{n}\"><span class=\"cite\">{cite}</span>\
<span class=\"visually-hidden\">: the evidence for {lower}</span></button></td>\
<td class=\"ours\" data-label=\"Devagotchi\">{ours}</td></tr>",
                what = row.what,
                dc = row.dc,
                cite = row.cite,
                n = i + 1,
                lower = row.what.to_lowercase(),
            )
        })
        .collect();
    let evidence: String = ROWS
        .iter()
        .enumerate()
        .map(|(i, row)| popup(i + 1, row))
        .collect();
    format!(
        "<section class=\"band\" id=\"compare\" aria-labelledby=\"compare-title\">\
<h2 id=\"compare-title\">Desktop Commander is #1. Devagotchi gives your AI the whole secure \
space.</h2>\
<p class=\"intro\">Desktop Commander puts a computer in the chat. Devagotchi is built for \
everything around that: every machine you have, a hub you own, and you in the loop. Side by \
side, from Desktop Commander's own source. Each § opens its evidence.</p>\
<div class=\"compare-wrap\"><table class=\"compare\"><thead><tr><th scope=\"col\"><span \
class=\"visually-hidden\">What</span></th><th scope=\"col\">Desktop Commander</th>\
<th scope=\"col\" class=\"ours\">Devagotchi</th></tr></thead><tbody>{rows}</tbody></table></div>\
<p class=\"source\">Desktop Commander is ahead today on macOS, which Devagotchi does not run \
on yet, and on PDF, Excel and Word files (§1.9). Its facts here are read from its source at \
commit c2b8795 (1 November 2026); § marks a section of \
<a href=\"{REPO}/blob/main/docs/desktop-commander-teardown.md\">our teardown</a>, where every \
claim is cited to a file and line, and plan §14 the comparison in \
<a href=\"{REPO}/blob/main/docs/product-and-platform-plan.md\">our plan</a>.</p>{evidence}</section>"
    )
}

fn popup(n: usize, row: &Row) -> String {
    let (doc, anchor, label) = match row.anchor.split_once(':') {
        Some(("p", anchor)) => (
            "product-and-platform-plan.md",
            anchor,
            "Read §14 of the plan",
        ),
        Some((_, anchor)) => (
            "desktop-commander-teardown.md",
            anchor,
            "Read the section in the teardown",
        ),
        None => unreachable!("every row names its anchor"),
    };
    modal(
        &format!("ev-{n}"),
        row.what,
        &format!(
            "<p class=\"modal-kicker\">Desktop Commander, {cite}</p>{evidence}\
<p class=\"modal-ours\"><strong>Devagotchi:</strong> {ours}</p>\
<p class=\"modal-source\"><a href=\"{REPO}/blob/main/docs/{doc}#{anchor}\">{label}</a>, \
where every claim is cited to a file and line of Desktop Commander at commit c2b8795.</p>",
            cite = row.cite,
            evidence = row.evidence,
            ours = row.ours,
        ),
    )
}
