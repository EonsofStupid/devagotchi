//! The words on the landing page a newcomer may not know, each with a short definition
//! shown as a tooltip: on hover and on keyboard focus (and a tap focuses it on a phone),
//! read by screen readers through `aria-describedby`. CSS only (`.term`, `.tip` in
//! `hub.css`), so the plain pages' CSP holds.

/// A glossary entry: its key (in the tooltip's id) and its definition.
#[derive(Clone, Copy)]
pub enum Term {
    Mcp,
    Passkey,
    Broker,
    Link,
    Audit,
    OAuth,
    Digest,
    ClaimCode,
}

impl Term {
    #[cfg(test)]
    pub const ALL: [Term; 8] = [
        Term::Mcp,
        Term::Passkey,
        Term::Broker,
        Term::Link,
        Term::Audit,
        Term::OAuth,
        Term::Digest,
        Term::ClaimCode,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Term::Mcp => "mcp",
            Term::Passkey => "passkey",
            Term::Broker => "broker",
            Term::Link => "link",
            Term::Audit => "audit",
            Term::OAuth => "oauth",
            Term::Digest => "digest",
            Term::ClaimCode => "claim-code",
        }
    }

    pub fn definition(self) -> &'static str {
        match self {
            Term::Mcp => {
                "Model Context Protocol: the open standard AI apps use to call tools on other \
systems. Your hub is one MCP server, at one address, for all of your machines."
            }
            Term::Passkey => {
                "A sign-in key your phone or computer keeps and unlocks with your face, \
fingerprint or PIN. There is no password to steal, guess or reuse."
            }
            Term::Broker => {
                "The Devagotchi service on each machine. It runs what your AI asks as jobs it \
owns, under the machine's own Devagotchi account, and keeps their output on the machine."
            }
            Term::Link => {
                "The machine's one connection to your hub: a WebSocket the machine opens \
outward and keeps. Calls come down it, so the machine opens no port."
            }
            Term::Audit => {
                "Every call is a row, and each row carries a hash of the row before it, so \
changing or deleting an old row breaks the chain where it happened."
            }
            Term::OAuth => {
                "The standard way an app gets permission to act for you without your \
password. The app asks the hub, you approve it once, and it gets a token you can end."
            }
            Term::Digest => {
                "The SHA-256 fingerprint of one exact container image. Pinned by digest, the \
hub runs precisely that build, not whatever a tag points at today."
            }
            Term::ClaimCode => {
                "A one-time code a hub with no owner prints to its own log. Reading the log \
takes the server, so only whoever runs the hub can claim it. It works for a day."
            }
        }
    }
}

/// `shown`, marked as `term` with its definition as a tooltip. `n` keeps the tooltip's id
/// unique when a term is marked more than once on a page.
pub fn term(term: Term, shown: &str, n: u8) -> String {
    let id = format!("tip-{}-{n}", term.key());
    format!(
        "<span class=\"term\" tabindex=\"0\" aria-describedby=\"{id}\">{shown}\
<span class=\"tip\" role=\"tooltip\" id=\"{id}\">{}</span></span>",
        term.definition()
    )
}
