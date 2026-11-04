//! Workflows in the store (migration 0010): one row per workflow an AI started for its
//! person. Every query runs in a transaction scoped to the workflow's tenant; a link finds
//! its tenant only through `Lookup::Workflow` (`devagotchi.workflow_tenant` on Postgres).
//! What each step did is kept in `progress`, with the browsers that did a step through the
//! link (by the SHA-256 of their cookie); starting, each line handed out, each step done,
//! completion, cancelling and a browser keeping the account are on the tenant's chain, in
//! the transaction that does them. The engine is `crate::workflows`.

use serde::{Deserialize, Serialize};

use super::*;
use crate::site::Workflow;

/// What a workflow's steps have done so far, by step index.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    #[serde(default)]
    pub steps: Vec<StepProgress>,
    /// The browsers that did a step through the link, newest last, at most
    /// [`BROWSERS_MAX`]: only they may keep the account afterwards.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browsers: Vec<BrowserMark>,
}

/// The browsers one workflow remembers; past it the oldest goes.
pub const BROWSERS_MAX: usize = 8;

/// A browser that did a step: the SHA-256 of the secret in its cookie, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserMark {
    pub hash: String,
    pub at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepProgress {
    /// When the hub first saw the step done; a done step stays done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<u64>,
    /// A machine step: the OS picked, the machine's stored name, and when its first join
    /// code was issued (a machine that enrolled since then is this workflow's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at: Option<u64>,
}

impl Progress {
    /// Step `index`'s progress, made if missing.
    pub fn step_mut(&mut self, index: usize) -> &mut StepProgress {
        if self.steps.len() <= index {
            self.steps.resize(index + 1, StepProgress::default());
        }
        &mut self.steps[index]
    }

    pub fn step(&self, index: usize) -> Option<&StepProgress> {
        self.steps.get(index)
    }

    /// Remember a browser that did a step (once; the newest [`BROWSERS_MAX`]).
    pub fn mark_browser(&mut self, hash: &str, now: u64) {
        self.browsers.retain(|mark| mark.hash != hash);
        self.browsers.push(BrowserMark {
            hash: hash.to_string(),
            at: now,
        });
        let extra = self.browsers.len().saturating_sub(BROWSERS_MAX);
        self.browsers.drain(..extra);
    }

    /// Whether the browser whose cookie hashes to `hash` did a step here within `window`
    /// seconds of `now`.
    pub fn did_a_step(&self, hash: &str, window: u64, now: u64) -> bool {
        self.browsers
            .iter()
            .any(|mark| mark.hash == hash && now < mark.at.saturating_add(window))
    }

    /// Whether any step was seen done: the first success.
    pub fn any_done(&self) -> bool {
        self.steps.iter().any(|step| step.done_at.is_some())
    }
}

/// One workflow as stored.
#[derive(Debug, Clone)]
pub struct WorkflowRow {
    pub tenant: String,
    pub id: String,
    pub account: String,
    pub definition: Workflow,
    pub machine_name: Option<String>,
    /// `open`, `completed` or `cancelled`.
    pub state: String,
    pub progress: Progress,
    pub expires_at: u64,
}


/// A new workflow's id: `wf_` and 96 random bits in hex.
pub fn new_workflow_id(random: [u8; 12]) -> String {
    format!("wf_{}", hex::encode(random))
}

/// A workflow id as the hub makes them.
pub fn valid_workflow_id(id: &str) -> bool {
    id.strip_prefix("wf_")
        .is_some_and(|hex| hex.len() == 24 && hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
}

/// A link token: `dgw_` and 256 random bits, base64url. Only its SHA-256 is kept.
pub fn new_link_token(random: [u8; 32]) -> String {
    format!("dgw_{}", URL_SAFE_NO_PAD.encode(random))
}

/// A link token's shape, checked before anything is looked up.
pub fn valid_link_token(token: &str) -> bool {
    token.strip_prefix("dgw_").is_some_and(|rest| {
        rest.len() == 43 && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

/// The key a link is stored under.
pub fn link_hash(token: &str) -> String {
    hash(token)
}

/// A browser's workflow cookie: `dgb_` and 256 random bits, base64url. Only its SHA-256 is
/// kept (`Progress::browsers`).
pub fn new_browser_secret(random: [u8; 32]) -> String {
    format!("dgb_{}", URL_SAFE_NO_PAD.encode(random))
}

/// The key a browser's cookie is remembered under, when it has the shape the hub gives.
pub fn browser_hash(secret: &str) -> Option<String> {
    secret
        .strip_prefix("dgb_")
        .filter(|rest| {
            rest.len() == 43
                && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .map(|_| hash(secret))
}

macro_rules! workflow_columns {
    () => {
        "tenant_id, id, account, definition, machine_name, state, progress, expires_at"
    };
}

fn workflow_row(row: &AnyRow) -> Result<WorkflowRow, sqlx::Error> {
    let damaged = |what: &str| sqlx::Error::Decode(format!("workflow {what} unreadable").into());
    let definition: String = row.try_get(3)?;
    let progress: String = row.try_get(6)?;
    Ok(WorkflowRow {
        tenant: row.try_get(0)?,
        id: row.try_get(1)?,
        account: row.try_get(2)?,
        definition: serde_json::from_str(&definition).map_err(|_| damaged("definition"))?,
        machine_name: row.try_get(4)?,
        state: row.try_get(5)?,
        progress: serde_json::from_str(&progress).map_err(|_| damaged("progress"))?,
        expires_at: unsecs(row.try_get(7)?),
    })
}

async fn workflow_in(
    connection: &mut AnyConnection,
    tenant: &str,
    id: &str,
) -> Result<Option<WorkflowRow>, sqlx::Error> {
    sqlx::query(concat!(
        "SELECT ",
        workflow_columns!(),
        " FROM tenancy_workflows WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant)
    .bind(id)
    .fetch_optional(&mut *connection)
    .await?
    .as_ref()
    .map(workflow_row)
    .transpose()
}

async fn save_progress(
    connection: &mut AnyConnection,
    row: &WorkflowRow,
    progress: &Progress,
) -> Result<(), Failed> {
    let text = serde_json::to_string(progress)
        .map_err(|error| BridgeError::new("account_store", error.to_string()))?;
    sqlx::query("UPDATE tenancy_workflows SET progress = $1 WHERE tenant_id = $2 AND id = $3")
        .bind(text)
        .bind(&row.tenant)
        .bind(&row.id)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

fn unknown_workflow() -> BridgeError {
    BridgeError::new("unknown_workflow", "no workflow with that id was started for this account")
}

/// What `note_done` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Noted {
    /// The steps it marked done now.
    pub steps: Vec<usize>,
    /// Whether the workflow completed now.
    pub completed: bool,
}

impl Accounts {
    /// Start `definition` for `account` of `tenant`: a new workflow and its link, live for
    /// `ttl` seconds. An open one of the same workflow for the account is cancelled first, so
    /// one link works at a time. Returns the workflow's id and the link's token.
    #[allow(clippy::too_many_arguments)]
    pub async fn start_workflow(
        &self,
        tenant: &str,
        account: &str,
        definition: &Workflow,
        machine_name: Option<&str>,
        ttl: u64,
        id_random: [u8; 12],
        link_random: [u8; 32],
        now: u64,
    ) -> BridgeResult<(String, String, u64)> {
        let id = new_workflow_id(id_random);
        let token = new_link_token(link_random);
        let text = serde_json::to_string(definition)
            .map_err(|error| BridgeError::new("account_store", error.to_string()))?;
        let expires_at = now + ttl;
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            let stale: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM tenancy_workflows
                 WHERE tenant_id = $1 AND account = $2 AND workflow = $3 AND state = 'open'",
            )
            .bind(tenant)
            .bind(account)
            .bind(&definition.id)
            .fetch_all(&mut *connection)
            .await?;
            for old in &stale {
                sqlx::query(
                    "UPDATE tenancy_workflows SET state = 'cancelled', ended_at = $1
                     WHERE tenant_id = $2 AND id = $3",
                )
                .bind(secs(now))
                .bind(tenant)
                .bind(old)
                .execute(&mut *connection)
                .await?;
                tenants::record_in(
                    connection,
                    tenant,
                    account,
                    "workflow.cancelled",
                    &format!("{old}: a new link replaced it"),
                    now,
                )
                .await?;
            }
            sqlx::query(
                "INSERT INTO tenancy_workflows (tenant_id, id, account, workflow, definition,
                   link_hash, machine_name, state, progress, created_at, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 'open', '{}', $8, $9)",
            )
            .bind(tenant)
            .bind(&id)
            .bind(account)
            .bind(&definition.id)
            .bind(&text)
            .bind(link_hash(&token))
            .bind(machine_name)
            .bind(secs(now))
            .bind(secs(expires_at))
            .execute(&mut *connection)
            .await?;
            tenants::record_in(
                connection,
                tenant,
                account,
                "workflow.started",
                &format!("{id}: {}", definition.id),
                now,
            )
            .await?;
            Ok(())
        })?;
        Ok((id, token, expires_at))
    }

    /// The workflow `id` of `tenant`, if it has one.
    pub async fn workflow(&self, tenant: &str, id: &str) -> BridgeResult<Option<WorkflowRow>> {
        if !valid_workflow_id(id) {
            return Ok(None);
        }
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            Ok(workflow_in(connection, tenant, id).await?)
        })
    }

    /// The workflow a link's token names, whatever its state; `None` for no such link.
    pub async fn workflow_by_link(&self, token: &str) -> BridgeResult<Option<WorkflowRow>> {
        if !valid_link_token(token) {
            return Ok(None);
        }
        let key = link_hash(token);
        transaction!(&self.store, |connection| {
            let Some(tenant) = self
                .enter(connection, crate::store::Lookup::Workflow(&key))
                .await?
            else {
                return Ok(None);
            };
            Ok(sqlx::query(concat!(
                "SELECT ",
                workflow_columns!(),
                " FROM tenancy_workflows WHERE tenant_id = $1 AND link_hash = $2"
            ))
            .bind(&tenant)
            .bind(&key)
            .fetch_optional(&mut *connection)
            .await?
            .as_ref()
            .map(workflow_row)
            .transpose()?)
        })
    }

    /// Mark `steps` of an open workflow done (each once, on the chain), and, `complete`,
    /// the workflow completed: its link stops working. Safe to run from any number of
    /// places at once; each step and the completion are recorded once.
    pub async fn note_done(
        &self,
        tenant: &str,
        id: &str,
        steps: &[(usize, &str)],
        complete: bool,
        now: u64,
    ) -> BridgeResult<Noted> {
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            let Some(row) = workflow_in(connection, tenant, id).await? else {
                return Err(unknown_workflow().into());
            };
            let mut noted = Noted::default();
            if row.state != "open" {
                return Ok(noted);
            }
            let mut progress = row.progress.clone();
            for (index, kind) in steps {
                let step = progress.step_mut(*index);
                if step.done_at.is_none() {
                    step.done_at = Some(now);
                    noted.steps.push(*index);
                    tenants::record_in(
                        connection,
                        tenant,
                        &row.account,
                        "workflow.step_done",
                        &format!("{id}: step {} ({kind})", index + 1),
                        now,
                    )
                    .await?;
                }
            }
            if progress != row.progress {
                save_progress(connection, &row, &progress).await?;
            }
            if complete {
                sqlx::query(
                    "UPDATE tenancy_workflows SET state = 'completed', ended_at = $1
                     WHERE tenant_id = $2 AND id = $3 AND state = 'open'",
                )
                .bind(secs(now))
                .bind(tenant)
                .bind(id)
                .execute(&mut *connection)
                .await?;
                tenants::record_in(
                    connection,
                    tenant,
                    &row.account,
                    "workflow.completed",
                    &format!("{id}: {}", row.definition.id),
                    now,
                )
                .await?;
                noted.completed = true;
            }
            Ok(noted)
        })
    }

    /// A machine step's join line was handed out, to the browser whose cookie hashes to
    /// `browser`: its OS and machine, and when the first code for that machine was issued
    /// (kept across new codes for the same machine).
    #[allow(clippy::too_many_arguments)]
    pub async fn note_machine(
        &self,
        tenant: &str,
        id: &str,
        index: usize,
        os: &str,
        stored: &str,
        shown: &str,
        browser: &str,
        now: u64,
    ) -> BridgeResult<()> {
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            let Some(row) = workflow_in(connection, tenant, id).await? else {
                return Err(unknown_workflow().into());
            };
            let mut progress = row.progress.clone();
            let step = progress.step_mut(index);
            if step.machine.as_deref() != Some(stored) || step.issued_at.is_none() {
                step.issued_at = Some(now);
            }
            step.os = Some(os.to_string());
            step.machine = Some(stored.to_string());
            progress.mark_browser(browser, now);
            save_progress(connection, &row, &progress).await?;
            tenants::record_in(
                connection,
                tenant,
                &row.account,
                "workflow.command",
                &format!("{id}: step {}: a join line for {shown} ({os})", index + 1),
                now,
            )
            .await?;
            Ok(())
        })
    }

    /// Cancel an open workflow: its link stops working. False when it was not open.
    pub async fn cancel_workflow(&self, tenant: &str, id: &str, now: u64) -> BridgeResult<bool> {
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            let Some(row) = workflow_in(connection, tenant, id).await? else {
                return Err(unknown_workflow().into());
            };
            if row.state != "open" {
                return Ok(false);
            }
            sqlx::query(
                "UPDATE tenancy_workflows SET state = 'cancelled', ended_at = $1
                 WHERE tenant_id = $2 AND id = $3",
            )
            .bind(secs(now))
            .bind(tenant)
            .bind(id)
            .execute(&mut *connection)
            .await?;
            tenants::record_in(
                connection,
                tenant,
                &row.account,
                "workflow.cancelled",
                &format!("{id}: by its AI"),
                now,
            )
            .await?;
            Ok(true)
        })
    }

    /// A browser did a step of workflow `id` through its link (turned a phone on): it is
    /// remembered, by the SHA-256 of its cookie, as one that may keep the account.
    pub async fn note_browser(&self, tenant: &str, id: &str, browser: &str, now: u64) -> BridgeResult<()> {
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            let Some(row) = workflow_in(connection, tenant, id).await? else {
                return Err(unknown_workflow().into());
            };
            let mut progress = row.progress.clone();
            progress.mark_browser(browser, now);
            save_progress(connection, &row, &progress).await?;
            Ok(())
        })
    }

    /// The browser whose cookie hashes to `browser` gets a session of workflow `id`'s account
    /// for its `ai_client` step, so the client's sign-in page there lands on this account:
    /// while the link is live, and only for an account nobody has secured yet (a secured
    /// one signs in on the client's page). The browser is remembered as one that did a step.
    /// Returns the session's token.
    pub async fn client_session(
        &self,
        tenant: &str,
        id: &str,
        browser: &str,
        client: &str,
        session_random: [u8; 32],
        now: u64,
    ) -> BridgeResult<String> {
        let refused = |code: &str, message: &str| Failed::Refused(BridgeError::new(code, message));
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            let Some(row) = workflow_in(connection, tenant, id).await? else {
                return Err(unknown_workflow().into());
            };
            if row.state != "open" || now >= row.expires_at {
                return Err(refused("expired", "the workflow's link no longer works"));
            }
            let holder = account_in(connection, &row.account)
                .await?
                .ok_or_else(|| BridgeError::new("no_account", "the workflow's account is gone"))?;
            if holder.disabled_at.is_some() {
                return Err(account_disabled(&holder.id).into());
            }
            if !holder.provisional {
                return Err(refused("secured", "this account is secured: sign in to it"));
            }
            let mut progress = row.progress.clone();
            progress.mark_browser(browser, now);
            save_progress(connection, &row, &progress).await?;
            let session =
                sessions::insert_session(connection, tenant, &holder.id, None, session_random, now).await?;
            tenants::record_in(
                connection,
                tenant,
                &holder.id,
                "session.opened",
                &format!("a browser to connect {client} through workflow {id}"),
                now,
            )
            .await?;
            Ok(session)
        })
    }

    /// The browser whose cookie hashes to `browser` keeps the account of workflow `id`: a
    /// session of that account, opened in the same transaction that checks it may. Only a
    /// browser that did a step through the link within `window` seconds, only once a step
    /// was seen done (the first success), and only for an account nobody has secured yet:
    /// the browser then secures it (a passkey, a linked sign-in). Returns the session's token.
    pub async fn keep_session(
        &self,
        tenant: &str,
        id: &str,
        browser: &str,
        window: u64,
        session_random: [u8; 32],
        now: u64,
    ) -> BridgeResult<String> {
        let refused = |code: &str, message: &str| Failed::Refused(BridgeError::new(code, message));
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            let Some(row) = workflow_in(connection, tenant, id).await? else {
                return Err(unknown_workflow().into());
            };
            if row.state == "cancelled" || !row.progress.did_a_step(browser, window, now) {
                return Err(refused("not_this_browser", "only the browser that did a step here keeps the account"));
            }
            if !row.progress.any_done() {
                return Err(refused("too_soon", "the account is kept once a step is done"));
            }
            let holder = account_in(connection, &row.account)
                .await?
                .ok_or_else(|| BridgeError::new("no_account", "the workflow's account is gone"))?;
            if holder.disabled_at.is_some() {
                return Err(account_disabled(&holder.id).into());
            }
            if !holder.provisional {
                return Err(refused("secured", "this account is secured: sign in to it"));
            }
            let session =
                sessions::insert_session(connection, tenant, &holder.id, None, session_random, now).await?;
            tenants::record_in(
                connection,
                tenant,
                &holder.id,
                "session.opened",
                &format!("the browser that did a step of workflow {id}, to keep the account"),
                now,
            )
            .await?;
            Ok(session)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_tokens_and_cookies_have_their_shapes() {
        let id = new_workflow_id([0xab; 12]);
        assert!(valid_workflow_id(&id), "{id}");
        for bad in ["wf_", "wf_ABABABABABABABABABABABAB", "acct_x", "wf_abababababababababababab0"] {
            assert!(!valid_workflow_id(bad), "{bad}");
        }
        let token = new_link_token([7; 32]);
        assert!(valid_link_token(&token), "{token}");
        assert!(!valid_link_token("dgw_short"));
        assert!(!valid_link_token(&token.replace("dgw_", "dgs_")));
        assert_eq!(link_hash(&token).len(), 64);
        let secret = new_browser_secret([9; 32]);
        assert_eq!(browser_hash(&secret).map(|key| key.len()), Some(64));
        for bad in ["dgb_short", "dgs_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", ""] {
            assert_eq!(browser_hash(bad), None, "{bad}");
        }
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn browsers_are_remembered_once_newest_last_and_only_for_a_while() {
        let mut progress = Progress::default();
        assert!(!progress.any_done() && !progress.did_a_step("a", 60, 0));
        progress.mark_browser("a", 10);
        progress.mark_browser("b", 20);
        progress.mark_browser("a", 30);
        assert_eq!(
            progress.browsers.iter().map(|m| (m.hash.as_str(), m.at)).collect::<Vec<_>>(),
            [("b", 20), ("a", 30)]
        );
        assert!(progress.did_a_step("a", 60, 89) && !progress.did_a_step("a", 60, 90));
        assert!(!progress.did_a_step("c", 60, 30));
        for n in 0..20 {
            progress.mark_browser(&format!("x{n}"), 100 + n);
        }
        assert_eq!(progress.browsers.len(), BROWSERS_MAX);
        assert_eq!(progress.browsers.last().unwrap().hash, "x19");
        progress.step_mut(2).done_at = Some(5);
        assert!(progress.any_done());
    }
}
