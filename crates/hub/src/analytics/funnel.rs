//! A tenant's milestones: each recorded once, with the tenant's id, where it happens. The
//! operator's funnel is visitors, then accounts made, secured, admitted, with a first
//! machine, with a first operation (`web/operator` reads them). Its first workflow started
//! and first completed (`workflows.rs`) are kept beside them.
//!
//! Never for the home tenant (the operator's own), and only on a hub that counts
//! (`HubConfig::counts_visits`): the account store writes its own in the transaction that
//! does the step (`Accounts::counting`), the rest go through `Analytics::event`.

use sqlx::AnyConnection;

use crate::store::secs;

/// An account was made, with the AI client family that made it ([`FAMILIES`]).
pub const MADE: &str = "made";
/// An account was secured, with the sign-in that did it (`passkey`, or a provider's name).
pub const SECURED: &str = "secured";
/// The tenant joined the early-access list.
pub const WAITING: &str = "waiting";
/// The tenant was let in (at once, when sign-up is open).
pub const ADMITTED: &str = "admitted";
/// Its first machine enrolled.
pub const MACHINE: &str = "machine";
/// Its first operation ran (`last_at`: its latest, a day at a time).
pub const OPERATION: &str = "operation";
/// Its first board was made.
pub const BOARD: &str = "board";
/// Its AI started its first workflow (`workflow_start`).
pub const WORKFLOW: &str = "workflow";
/// Its first workflow was completed.
pub const WORKFLOW_DONE: &str = "workflow_done";

/// Every milestone, in the funnel's order where it has one.
#[cfg_attr(not(test), allow(dead_code))]
pub const KINDS: [&str; 9] = [
    MADE,
    SECURED,
    WAITING,
    ADMITTED,
    MACHINE,
    OPERATION,
    BOARD,
    WORKFLOW,
    WORKFLOW_DONE,
];

/// The AI client families an account is made by.
#[cfg_attr(not(test), allow(dead_code))]
pub const FAMILIES: [&str; 5] = ["claude", "chatgpt", "grok", "cursor", "other"];

/// The family of the AI client an OAuth client's name says it is: Claude, ChatGPT or Codex,
/// Grok, Cursor, or another (a terminal, a browser sign-in, anything else).
pub fn client_family(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    let has = |word: &str| name.contains(word);
    if has("claude") {
        "claude"
    } else if has("chatgpt") || has("codex") || has("openai") {
        "chatgpt"
    } else if has("grok") {
        "grok"
    } else if has("cursor") {
        "cursor"
    } else {
        "other"
    }
}

/// A milestone's detail as kept: at most 64 characters of `[a-z0-9_.-]`.
fn detail(text: &str) -> String {
    text.chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        .take(64)
        .collect()
}

/// Record `tenant`'s milestone `kind` at `now`, inside the caller's transaction: the first
/// time only (an operation's latest moves on), and never for the home tenant or a tenant
/// the hub does not have.
pub async fn note_in(
    connection: &mut AnyConnection,
    tenant: &str,
    kind: &str,
    text: &str,
    now: u64,
) -> Result<(), sqlx::Error> {
    macro_rules! insert {
        ($on_conflict:literal) => {
            concat!(
                "INSERT INTO hub_analytics_events (tenant_id, kind, detail, at, last_at)
                 SELECT $1, $2, $3, $4, $4
                 WHERE EXISTS (SELECT 1 FROM tenancy_tenants WHERE id = $1 AND home = 0) ",
                $on_conflict
            )
        };
    }
    let sql = if kind == OPERATION {
        insert!(
            "ON CONFLICT (tenant_id, kind) DO UPDATE SET last_at = excluded.last_at
             WHERE hub_analytics_events.last_at < excluded.last_at"
        )
    } else {
        insert!("ON CONFLICT (tenant_id, kind) DO NOTHING")
    };
    sqlx::query(sql)
        .bind(tenant)
        .bind(kind)
        .bind(detail(text))
        .bind(secs(now))
        .execute(&mut *connection)
        .await?;
    Ok(())
}
