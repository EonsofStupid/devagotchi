//! The landing page's popups beside the comparison's evidence (`compare.rs`): how claiming
//! works, every tool the AI gets, and the setup walkthrough. Each is a `popover` opened by a
//! `popovertarget` button: no script, light dismiss, Escape closes it.

use super::modal;

/// How a hub with no owner is claimed (`accounts/claim.rs`, `web/claim.rs`), in plain words.
/// Never names the claim page's path: a claimed hub's page offers no claim.
pub fn claim() -> String {
    modal(
        "m-claim",
        "How claiming works",
        "<ol class=\"modal-steps\">\
<li><strong>The hub prints a claim link.</strong> A hub with no owner prints one to its own log \
each time it starts. Reading that log takes the server, or the platform its container runs on, \
so someone who only found the address cannot claim it.</li>\
<li><strong>Open it where your passkey will live.</strong> On your phone or your computer. The \
code rides after the <code>#</code> in the link, which a browser never sends, so it stays out of \
every request and every log. You can also type the code in.</li>\
<li><strong>Name the owner and make the passkey.</strong> One button does both, and you are \
signed in as the hub's owner.</li>\
<li><strong>The code is gone.</strong> It is deleted in the same step that stores your passkey, \
and from then on the hub refuses every claim.</li></ol>\
<p>The code is 120 random bits, and the hub keeps only its SHA-256. It works for a day; \
restarting the hub prints a new one, and only the newest works. A cancelled passkey prompt is \
retried with the same code.</p>\
<p class=\"modal-source\">After that: add your machines from your account, and approve each AI \
app once with your passkey. The rest is in the <a href=\"/docs\">docs</a>.</p>",
    )
}

/// The hub's MCP tools, grouped by what they are for. A test holds this to the tools the hub
/// serves (`hub::tool_names`).
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

pub fn tools() -> String {
    let groups: String = TOOLS
        .iter()
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
            format!("<h3>{group}</h3><dl class=\"tools\">{rows}</dl>")
        })
        .collect();
    modal(
        "m-tools",
        "What the AI can do",
        &format!(
            "<p>Every tool your AI gets from this hub. Each machine still decides what may run \
there: only the programs you granted, or anything its own account can.</p>{groups}"
        ),
    )
}

/// The setup, step by step: what the video shows. Only what is built.
pub fn watch() -> String {
    modal(
        "m-watch",
        "Watch it set up",
        "<div class=\"video-slot\"><p>The video of this setup is being made. Here is what it \
shows, in order:</p></div>\
<ol class=\"modal-steps\">\
<li><strong>One command on a server.</strong> <code>docker compose up -d</code>, or \
<code>install.sh</code> on Debian and Ubuntu.</li>\
<li><strong>The log prints a claim link.</strong></li>\
<li><strong>Your phone opens it.</strong> Name yourself, make your passkey, and you are signed \
in as the owner.</li>\
<li><strong>Claude connects.</strong> Add a custom connector at your hub's MCP address, approve \
it on the hub's consent page, and it is connected.</li>\
<li><strong>Add a machine.</strong> One line on that machine, and it shows up online.</li>\
<li><strong>Your AI asks first.</strong> It asks to restart a service; the approval reaches your \
phone; you approve, and it is done.</li></ol>",
    )
}
