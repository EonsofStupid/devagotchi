//! Workflows with direct links (increment 68): the AI does the work, its person just clicks.
//!
//! An AI asked to "connect my server" starts a workflow (`workflow_start`, `mcp.rs`) and
//! hands its person one link, `<hub>/go/<token>` (`web/workflow.rs`). The link is the
//! credential, as the line `machine_add` hands an AI already is: nobody signs in to follow
//! it. The page walks them through the workflow's steps and ticks each the moment the hub
//! sees it done, while the AI follows along with `workflow_wait` and carries on.
//!
//! - **Workflows are content, steps are code.** The site's `content/workflows.json` says
//!   which workflows there are and their words (`site/workflows.rs`); each step names a kind
//!   this module runs: `machine` (done when a machine enrolled through this workflow is
//!   online), `phone` (done when the account has a push subscription), `secure_account`
//!   (done when the account is secured; optional, it never holds a workflow up) and `done`
//!   (the closing step). A workflow keeps the definition it started with, so a site changed
//!   later never reshapes one under way.
//! - **Steps are seen, not claimed.** Whoever looks (the AI's tools, the page, its poll) gets
//!   the hub's own view: the machine's link, the account's phones, the account itself. A step
//!   seen done is recorded once, on the tenant's chain, and stays done; when every step that
//!   is not optional is, the workflow completes and its link stops granting anything.
//! - **A step the hub cannot do says why** (`unavailable`): a hub that hands out no builds
//!   (or none for that OS) has no one-line install, one that sends no notifications has no
//!   phone step, a tenant waiting to be let in or at its plan's machine limit adds no
//!   machine. Nobody is handed a command that cannot work.

pub(crate) mod mcp;

use std::sync::Arc;

use devagotchi_contracts::{BridgeError, BridgeResult};
use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    accounts::{
        Accounts,
        workflows::{StepProgress, WorkflowRow},
    },
    auth::Subject,
    hub::Hub,
    site::{
        Ctx, Site,
        workflows::{DONE, MACHINE, MACHINE_OS, PHONE, SECURE_ACCOUNT},
    },
};

/// The workflows of a hub with its own accounts: their store, how long a link works, and
/// what wakes a wait.
pub struct Workflows {
    accounts: Arc<Accounts>,
    changes: Arc<tokio::sync::Notify>,
    link_ttl: u64,
}

impl Workflows {
    pub fn new(accounts: Arc<Accounts>, changes: Arc<tokio::sync::Notify>, link_ttl: u64) -> Self {
        Self {
            accounts,
            changes,
            link_ttl,
        }
    }

    pub fn accounts(&self) -> &Arc<Accounts> {
        &self.accounts
    }

    pub fn link_ttl(&self) -> u64 {
        self.link_ttl
    }

    /// Something a workflow may be waiting on changed on this hub.
    pub fn changed(&self) {
        self.changes.notify_waiters();
    }

    pub fn changes(&self) -> &tokio::sync::Notify {
        &self.changes
    }
}

/// One step as the hub sees it now.
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct StepStatus {
    /// `secure_account`, `machine`, `phone` or `done`.
    pub kind: String,
    pub title: String,
    /// `done`, `current` (the one to do now), `pending` or `unavailable` (see `why`).
    pub state: String,
    /// What the hub sees, in words for the user: "nas is online".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fact: Option<String>,
    /// A machine step: the machine's name, once its line was handed out (or the name it
    /// will get).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// A machine step: `linux`, `windows`, `docker` or `host`, fixed or as the user picked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// Why an unavailable step cannot be done: tell the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// The step is offered but never holds the workflow up (keeping the account).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

/// Where a workflow stands.
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WorkflowStatus {
    pub id: String,
    /// The workflow's id from `workflow_list`.
    pub workflow: String,
    pub title: String,
    /// `open`, `completed`, `cancelled` or `expired` (the link stopped working: start again).
    pub state: String,
    /// The link to give the user, while it works.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// When the link stops working, in Unix seconds.
    pub expires_at: u64,
    pub steps: Vec<StepStatus>,
    /// What to do next.
    pub next: String,
}

/// What the hub sees of one step, before the states are worked out.
struct Seen {
    done: bool,
    why: Option<String>,
    fact: Option<String>,
    machine: Option<String>,
    os: Option<String>,
}

/// A step's machine: whether it enrolled after its line was handed out, and is online.
pub(crate) enum MachineSeen {
    None,
    Waiting,
    Offline,
    Online,
}

impl Hub {
    /// The workflows of this hub, or why there are none: a hub without its own accounts.
    pub(crate) fn workflows(&self) -> Result<&Arc<Workflows>, BridgeError> {
        self.workflows.as_ref().ok_or_else(|| {
            BridgeError::new(
                "workflows_unavailable",
                "this hub has no accounts of its own, so it starts no workflows",
            )
        })
    }

    /// Why a machine step on `os` (any, when `None`) cannot be done on this hub: no builds
    /// to hand out, or none for that OS. Linux, Docker and a Proxmox or Incus host all run
    /// the Linux join script; Windows its PowerShell one. Each needs its installer and at
    /// least one archive, or its line could not work.
    pub(crate) fn machine_unavailable(&self, site: &Site, os: Option<&str>) -> Option<String> {
        let builds = |os: &str| {
            self.downloads.as_ref().is_some_and(|downloads| {
                let (installer, archives) = match os {
                    "windows" => (
                        "install-devagotchi.ps1",
                        ["devagotchi-x86_64-pc-windows-msvc.zip", "devagotchi-aarch64-pc-windows-msvc.zip"],
                    ),
                    _ => (
                        "install-devagotchi.sh",
                        [
                            "devagotchi-x86_64-unknown-linux-gnu.tar.xz",
                            "devagotchi-aarch64-unknown-linux-gnu.tar.xz",
                        ],
                    ),
                };
                downloads.sha256(installer).is_some()
                    && archives.iter().any(|archive| downloads.sha256(archive).is_some())
            })
        };
        let ok = match os {
            Some(os) => builds(os),
            None => MACHINE_OS.iter().any(|os| builds(os)),
        };
        let copy = &site.messages.workflow;
        (!ok).then(|| match (os, self.downloads.is_some()) {
            (Some(os), true) => Ctx::new(site)
                .with("os", Ctx::new(site).plain(copy.os.get(os).map(String::as_str).unwrap_or(os)))
                .plain(&copy.no_build_for),
            _ => Ctx::new(site).plain(&copy.no_downloads),
        })
    }

    /// The OSes a machine step can offer on this hub.
    pub(crate) fn machine_oses(&self) -> Vec<&'static str> {
        let site = self.config.site();
        MACHINE_OS
            .iter()
            .copied()
            .filter(|os| self.machine_unavailable(&site, Some(os)).is_none())
            .collect()
    }

    /// Why a workflow cannot be started on this hub, if it cannot: the site says so, or a
    /// step needs what this hub does not have.
    pub(crate) fn workflow_unavailable(&self, site: &Site, workflow: &crate::site::Workflow) -> Option<String> {
        if let Some(why) = &workflow.unavailable {
            return Some(Ctx::new(site).plain(why));
        }
        workflow.steps.iter().find_map(|step| match step.kind.as_str() {
            MACHINE => self.machine_unavailable(site, step.os.as_deref()),
            PHONE if self.push.is_none() => Some(Ctx::new(site).plain(&site.messages.workflow.no_push)),
            _ => None,
        })
    }

    /// The workflow's account as the hub sees it now, for a page that follows the link: no
    /// sign-in, the link is the credential. `None` when the account is gone or disabled.
    pub(crate) async fn workflow_subject(&self, row: &WorkflowRow) -> BridgeResult<Option<Subject>> {
        let workflows = self.workflows()?;
        let Some(mut account) = workflows
            .accounts()
            .get(&row.account)
            .await?
            .filter(|account| account.disabled_at.is_none())
        else {
            return Ok(None);
        };
        // Its tenant's place (let in, its plan), as a request of its own would see it.
        account.access = workflows.accounts().account_access(&account.id).await?;
        Ok(Some(workflows.accounts().subject(&account, "link")))
    }

    /// The machine a step handed out a line for: enrolled for the account since then, and
    /// online.
    pub(crate) fn machine_seen(&self, account: &str, progress: Option<&StepProgress>) -> MachineSeen {
        let (Some(stored), Some(issued)) = (
            progress.and_then(|p| p.machine.as_deref()),
            progress.and_then(|p| p.issued_at),
        ) else {
            return MachineSeen::None;
        };
        match self.links.device(stored) {
            Ok(Some(device))
                if device.owner.as_deref() == Some(account)
                    && device.revoked_at.is_none()
                    && device.enrolled_at >= issued =>
            {
                if self.links.connected(stored) {
                    MachineSeen::Online
                } else {
                    MachineSeen::Offline
                }
            }
            _ => MachineSeen::Waiting,
        }
    }

    /// The name a machine step's machine gets: the one already handed out, the AI's, or the
    /// site's for the OS, made free among the account's machines (`server`, `server-2`, …).
    pub(crate) fn machine_name_for(&self, subject: &Subject, row: &WorkflowRow, index: usize, os: &str) -> String {
        if let Some(stored) = row.progress.step(index).and_then(|p| p.machine.as_deref()) {
            return self.shown_name(subject, stored);
        }
        if let Some(name) = &row.machine_name {
            return name.clone();
        }
        let site = self.config.site();
        let base = site
            .messages
            .workflow
            .default_names
            .get(os)
            .cloned()
            .unwrap_or_else(|| "machine".into());
        let held: Vec<String> = self
            .own_machines(subject)
            .into_iter()
            .filter(|device| device.revoked_at.is_none())
            .map(|device| device.name)
            .collect();
        let free = |name: &str| !held.iter().any(|held| held == name) && self.config.broker(name).is_none();
        if free(&base) {
            return base;
        }
        (2..100)
            .map(|n| format!("{base}-{n}"))
            .find(|name| free(name))
            .unwrap_or(base)
    }

    /// Where `row` stands for its account `subject` at `now`, with every step the hub sees
    /// done recorded (once) and the workflow completed when all are. `link` is the page's
    /// address while it works (the AI's tools have it; the page does not need it).
    pub(crate) async fn look_at_workflow(
        &self,
        subject: &Subject,
        mut row: WorkflowRow,
        link: Option<String>,
        now: u64,
    ) -> BridgeResult<WorkflowStatus> {
        let workflows = self.workflows()?.clone();
        let site = self.config.site();
        let copy = &site.messages.workflow;
        let facts = &copy.facts;
        let ctx = || Ctx::new(&site);
        let phones = self
            .push
            .as_ref()
            .map(|push| push.devices().push_for(&subject.id).map(|p| p.len()).unwrap_or(0));
        let room = self.machine_room(subject).map(|refused| refused.message);
        let steps = row.definition.steps.clone();
        let mut seen: Vec<Seen> = Vec::with_capacity(steps.len());
        for (index, step) in steps.iter().enumerate() {
            let progress = row.progress.step(index);
            let recorded = progress.is_some_and(|p| p.done_at.is_some());
            let mut one = Seen {
                done: recorded,
                why: None,
                fact: None,
                machine: None,
                os: step.os.clone().or_else(|| progress.and_then(|p| p.os.clone())),
            };
            match step.kind.as_str() {
                SECURE_ACCOUNT => {
                    let secured = subject.provisional_until.is_none();
                    one.done |= secured;
                    one.fact = Some(ctx().plain(if one.done { &facts.secured } else { &facts.not_secured }));
                }
                MACHINE => {
                    let os = one.os.clone().unwrap_or_else(|| "linux".into());
                    let name = self.machine_name_for(subject, &row, index, &os);
                    let state = self.machine_seen(&subject.id, progress);
                    one.done |= matches!(state, MachineSeen::Online);
                    let fact = match (one.done, state) {
                        (true, _) | (_, MachineSeen::Online) => &facts.machine_online,
                        (_, MachineSeen::Offline) => &facts.machine_offline,
                        (_, MachineSeen::Waiting) => &facts.machine_waiting,
                        (_, MachineSeen::None) => &facts.machine_none,
                    };
                    one.fact = Some(ctx().with("machine", name.clone()).plain(fact));
                    one.machine = Some(name);
                    if !one.done && progress.and_then(|p| p.machine.as_deref()).is_none() {
                        one.why = self
                            .machine_unavailable(&site, step.os.as_deref())
                            .or_else(|| room.clone());
                    }
                }
                PHONE => match phones {
                    Some(n) => {
                        one.done |= n > 0;
                        one.fact = Some(if n > 0 {
                            ctx().with("n", n.to_string()).plain(&facts.phone_on)
                        } else {
                            ctx().plain(&facts.phone_off)
                        });
                    }
                    None if !one.done => one.why = Some(ctx().plain(&copy.no_push)),
                    None => {}
                },
                _ => {}
            }
            seen.push(one);
        }
        // The closing step is done when every step before it is, keeping the account aside:
        // it is offered, and never holds a workflow up.
        let all_done = steps
            .iter()
            .zip(&seen)
            .all(|(step, seen)| step.kind == DONE || step.kind == SECURE_ACCOUNT || seen.done);
        for (step, one) in steps.iter().zip(seen.iter_mut()) {
            if step.kind == DONE {
                one.done |= all_done;
                if one.done {
                    one.fact = Some(ctx().plain(&facts.done));
                }
            }
        }
        // Record what is newly seen done, once, and complete the workflow when all is.
        if row.state == "open" {
            let newly: Vec<(usize, &str)> = steps
                .iter()
                .zip(&seen)
                .enumerate()
                .filter(|(index, (_, seen))| {
                    seen.done && row.progress.step(*index).is_none_or(|p| p.done_at.is_none())
                })
                .map(|(index, (step, _))| (index, step.kind.as_str()))
                .collect();
            if !newly.is_empty() {
                let noted = workflows
                    .accounts()
                    .note_done(&row.tenant, &row.id, &newly, all_done, now)
                    .await?;
                for index in &noted.steps {
                    row.progress.step_mut(*index).done_at = Some(now);
                }
                if noted.completed {
                    row.state = "completed".into();
                    if let Some(analytics) = &self.analytics {
                        analytics.tenant_event(&row.tenant, crate::analytics::funnel::WORKFLOW_DONE);
                    }
                }
                workflows.changed();
            }
        }
        let state = match row.state.as_str() {
            "open" if now >= row.expires_at => "expired",
            other => other,
        }
        .to_string();
        let mut current = None;
        let statuses: Vec<StepStatus> = steps
            .iter()
            .zip(seen)
            .enumerate()
            .map(|(index, (step, seen))| {
                let optional = step.kind == SECURE_ACCOUNT;
                let state = if seen.done {
                    "done"
                } else if seen.why.is_some() {
                    "unavailable"
                } else if current.is_none() && step.kind != DONE && !optional {
                    current = Some(index);
                    "current"
                } else {
                    "pending"
                };
                StepStatus {
                    kind: step.kind.clone(),
                    title: ctx().plain(&step.title),
                    state: state.into(),
                    fact: seen.fact,
                    machine: seen.machine,
                    os: seen.os,
                    why: seen.why,
                    optional,
                }
            })
            .collect();
        let next = next_for(&state, &statuses);
        Ok(WorkflowStatus {
            id: row.id.clone(),
            workflow: row.definition.id.clone(),
            title: ctx().plain(&row.definition.title),
            link: link.filter(|_| state == "open"),
            expires_at: row.expires_at,
            state,
            steps: statuses,
            next,
        })
    }
}

/// What the AI does next, given where the workflow stands.
fn next_for(state: &str, steps: &[StepStatus]) -> String {
    match state {
        "completed" => "Done: tell the user, and carry on with what they asked (a machine it added is in `machines` now).".into(),
        "cancelled" => "Cancelled: its link no longer works. Start it again if the user still wants it.".into(),
        "expired" => "The link expired before the user finished: start the workflow again for a new link.".into(),
        _ => match steps.iter().find(|step| step.state == "current") {
            Some(step) => format!(
                "Waiting for the user on \"{}\" on the page the link opens. Call workflow_wait to follow it.",
                step.title
            ),
            None => match steps.iter().find(|step| step.state == "unavailable") {
                Some(step) => format!(
                    "\"{}\" cannot be done: tell the user why, plainly ({}), and offer no workaround.",
                    step.title,
                    step.why.as_deref().unwrap_or_default()
                ),
                None => "Call workflow_wait to follow it.".into(),
            },
        },
    }
}
