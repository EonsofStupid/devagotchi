//! The landing page's popups beside the comparison's evidence (`compare.rs`): how claiming
//! works, every tool the AI gets, and the setup walkthrough. Each is a `popover` opened by a
//! `popovertarget` button: no script, light dismiss, Escape closes it. A popup is on the
//! page only when something on it opens it.

use super::{Ctx, modal, video};

/// The popups something on this page opens, in the order they close the page.
pub fn all(ctx: &Ctx) -> String {
    let mut out = String::new();
    if ctx.opens("claim") {
        out.push_str(&claim(ctx));
    }
    if ctx.opens("tools") {
        out.push_str(&tools(ctx));
    }
    if ctx.opens("watch") {
        match video(ctx) {
            Some(video) => out.push_str(&watch(ctx, &video)),
            None => ctx
                .problem("{pop:watch} opens the setup video, and site.json names no assets.video"),
        }
    }
    out
}

/// `<li><strong>Title.</strong> Text</li>` for each step.
fn steps(ctx: &Ctx, steps: &[crate::site::Item]) -> String {
    let items: String = steps
        .iter()
        .map(|step| {
            let text = step
                .text
                .pick("default")
                .map(|t| ctx.inline(t))
                .unwrap_or_default();
            let text = if text.is_empty() {
                text
            } else {
                format!(" {text}")
            };
            format!("<li><strong>{}</strong>{text}</li>", ctx.inline(&step.term))
        })
        .collect();
    format!("<ol class=\"modal-steps\">{items}</ol>")
}

/// How a hub with no owner is claimed (`accounts/claim.rs`, `web/claim.rs`), in plain words.
fn claim(ctx: &Ctx) -> String {
    let copy = &ctx.site.landing.popups.claim;
    let paragraphs: String = copy
        .paragraphs
        .iter()
        .map(|p| format!("<p>{}</p>", ctx.inline(p)))
        .collect();
    let source = if copy.source.is_empty() {
        String::new()
    } else {
        format!("<p class=\"modal-source\">{}</p>", ctx.inline(&copy.source))
    };
    modal(
        "m-claim",
        &ctx.inline(&copy.title),
        &format!("{}{paragraphs}{source}", steps(ctx, &copy.steps)),
    )
}

/// The hub's MCP tools, grouped by what they are for. They are the hub's own, so they stay
/// with its code: a test holds this to the tools the hub serves (`hub::tool_names`).
pub const TOOLS: [(&str, &[(&[&str], &str)]); 5] = [
    (
        "Find your machines",
        &[
            (
                &["machines"],
                "Every machine your AI can work on, and whether each is online now. It starts here.",
            ),
            (
                &["machine_add"],
                "Adds a machine to your account: a one-time code and the line that installs it, for Linux, Windows, Docker Compose, Proxmox or Incus.",
            ),
            (
                &["whoami"],
                "Who it is signed in as, and the boards it may use.",
            ),
            (
                &["workflow_list", "workflow_start"],
                "Offers a setup (a server, a PC, your phone) and starts it as one link you open: no sign-in, one button, one line to paste.",
            ),
            (
                &["workflow_status", "workflow_wait", "workflow_cancel"],
                "Follows that link's steps and carries on the moment each one is done, or cancels the link.",
            ),
        ],
    ),
    (
        "Run commands",
        &[
            (
                &["run"],
                "Runs a command and returns its output in one call. Still running after the wait, it keeps running as a job.",
            ),
            (
                &["operation_start"],
                "Starts a job the machine's broker owns: for long work, or on a machine that runs only the programs you granted.",
            ),
            (
                &["operation_wait"],
                "Waits until a job finishes or prints something new.",
            ),
            (
                &["operation_get", "operation_read"],
                "A job's state, and its output after a cursor.",
            ),
            (
                &["operation_cancel"],
                "Stops a job. Disconnecting never does.",
            ),
        ],
    ),
    (
        "Work with files",
        &[
            (
                &["read_file", "read_multiple_files"],
                "Reads text files by lines, up to 20 in one call.",
            ),
            (&["read_image"], "Looks at a PNG, JPEG, GIF or WebP image."),
            (
                &["list_directory", "get_file_info"],
                "Lists a folder, or says what a path is without reading it.",
            ),
            (
                &["write_file", "edit_file"],
                "Writes a file whole, never half-written, or replaces exact text in one.",
            ),
            (
                &["create_directory", "move_file"],
                "Makes folders, and moves or renames on the same filesystem.",
            ),
        ],
    ),
    (
        "Ask you first",
        &[(
            &["approval_request", "approval_wait"],
            "Asks you to approve a step, with what will change, why and how to undo it, then waits for your answer.",
        )],
    ),
    (
        "Share a board with other agents",
        &[(
            &[
                "board_prime",
                "board_ready",
                "board_list",
                "board_get",
                "board_create",
                "board_claim",
                "board_heartbeat",
                "board_update",
                "board_note",
                "board_handoff",
                "board_release",
                "board_complete",
                "board_dep",
                "board_force_release",
                "board_events",
                "board_wait",
                "board_verify",
                "board_bind",
            ],
            "A project's ticket board: see what is ready, claim work and keep the claim alive, leave notes, hand off, finish.",
        )],
    ),
];

/// The offers' tools, listed under the site's network while its offers show
/// (`crate::offers`).
pub const OFFER_TOOLS: &[(&[&str], &str)] = &[(
    &["offers", "offers_accept"],
    "Says what the network offers, asked once after your first machine works, and links your account when you say yes.",
)];

fn tools(ctx: &Ctx) -> String {
    let copy = &ctx.site.landing.popups.tools;
    let network = ctx
        .site
        .info
        .offers()
        .map(|offers| (ctx.plain(&offers.brand), OFFER_TOOLS));
    let groups: String = TOOLS
        .iter()
        .map(|(group, tools)| (group.to_string(), *tools))
        .chain(network)
        .map(|(group, tools)| {
            let rows: String = tools
                .iter()
                .map(|(names, what)| {
                    let names: String = names
                        .iter()
                        .map(|name| format!("<code>{name}</code>"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("<dt>{names}</dt><dd>{what}</dd>")
                })
                .collect();
            format!(
                "<h3>{}</h3><dl class=\"tools\">{rows}</dl>",
                crate::site::text::escape_text(&group)
            )
        })
        .collect();
    modal(
        "m-tools",
        &ctx.inline(&copy.title),
        &format!("<p>{}</p>{groups}", ctx.inline(&copy.intro)),
    )
}

/// The setup video, and the same steps as text: what the video shows.
fn watch(ctx: &Ctx, video: &str) -> String {
    let copy = &ctx.site.landing.popups.watch;
    modal(
        "m-watch",
        &ctx.inline(&copy.title),
        &format!(
            "<div class=\"video-frame\">{video}</div>\
<p class=\"modal-source\">{}</p>{}",
            ctx.inline(&copy.caption),
            steps(ctx, &copy.steps)
        ),
    )
}
