//! The workflow tools: what an AI offers its person, starts for them as one link, follows
//! until each step is done, and cancels.

use std::time::Duration;

use devagotchi_contracts::BridgeError;
use rmcp::{
    Json, RoleServer, handler::server::wrapper::Parameters, service::RequestContext, tool,
    tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{StepStatus, WorkflowStatus};
use crate::{
    accounts::workflows::WorkflowRow, auth::Subject, devices::now_secs, hub::Hub, site::Ctx,
};

/// The longest a `workflow_wait` holds.
pub const WAIT_MAX_SECS: u64 = 60;
const WAIT_DEFAULT_SECS: u64 = 50;
/// How often a wait looks again when nothing on this hub woke it (a change another hub
/// instance saw, or one nothing signals: an account secured).
const LOOK_EVERY: Duration = Duration::from_secs(1);

#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkflowList {
    pub workflows: Vec<WorkflowOffer>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkflowOffer {
    /// What to pass as `workflow` to workflow_start.
    pub id: String,
    pub title: String,
    /// What it does, in words for the user.
    pub intro: String,
    /// False: it cannot be done on this hub yet; `why` says why. Never offer a workaround.
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    pub steps: Vec<StepOffer>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StepOffer {
    pub kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// An `ai_client` step's client: `claude`, `chatgpt`, `grok` or `cursor`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartIn {
    /// A workflow id from workflow_list.
    pub workflow: String,
    /// The name its machine step gives the machine (`[a-z0-9][a-z0-9-]*`); by default one
    /// for its OS that the account does not use yet (`server`, `pc`, …).
    #[serde(default)]
    pub machine_name: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkflowStarted {
    pub id: String,
    /// Give this to the user: the page it opens walks them through every step.
    pub link: String,
    /// When the link stops working, in Unix seconds.
    pub expires_at: u64,
    pub title: String,
    /// What it does, in words for the user.
    pub intro: String,
    pub steps: Vec<StepStatus>,
    pub next: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdIn {
    /// The workflow's id from workflow_start.
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitIn {
    /// The workflow's id from workflow_start.
    pub id: String,
    /// Seconds to wait for a change, 1 to 60 (default 50).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkflowWaited {
    /// False when the wait timed out with nothing changed.
    pub changed: bool,
    #[serde(flatten)]
    pub status: WorkflowStatus,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Cancelled {
    /// False when it was already completed or cancelled.
    pub cancelled: bool,
}

fn refused(code: &str, message: impl Into<String>) -> Json<BridgeError> {
    Json(BridgeError::new(code, message))
}

impl Hub {
    /// The page a workflow's link opens.
    fn workflow_link(&self, token: &str) -> String {
        format!("{}/go/{token}", self.config.public_url.trim_end_matches('/'))
    }

    /// Workflow `id` of `subject`'s: its tenant's, and started for this account. Anyone
    /// else's is not found, so ids of other accounts' workflows reveal nothing.
    async fn own_workflow(&self, subject: &Subject, id: &str) -> Result<WorkflowRow, Json<BridgeError>> {
        let workflows = self.workflows().map_err(Json)?;
        let not_found = || refused("unknown_workflow", "no workflow with that id was started for this account");
        let Some(tenant) = subject.tenant.as_deref() else {
            return Err(not_found());
        };
        match workflows.accounts().workflow(tenant, id.trim()).await.map_err(Json)? {
            Some(row) if row.account == subject.id => Ok(row),
            _ => Err(not_found()),
        }
    }

    /// Audit one workflow tool call like every other.
    fn record_workflow<T>(
        &self,
        subject: &Subject,
        tool: &str,
        id: &str,
        result: Result<T, Json<BridgeError>>,
    ) -> Result<T, Json<BridgeError>> {
        let outcome = match &result {
            Ok(_) => "ok".to_string(),
            Err(Json(error)) => error.code.clone(),
        };
        self.record(subject, tool, "", id, &outcome)?;
        result
    }
}

#[tool_router(router = workflow_router, vis = "pub(crate)")]
impl Hub {
    #[tool(
        name = "workflow_list",
        description = "The setups this hub can walk the user through with one link they open and click through, no sign-in: connecting a Linux server, a Windows PC or laptop, their phone for approvals, another AI app (Claude, ChatGPT or Codex, Grok, Cursor), or everything to get started. Use it when the user asks to connect, add or set up a machine, server, computer, laptop, phone or AI app, before workflow_start; prefer it to machine_add unless the user wants the raw command. A workflow with available false cannot be done here: tell the user its `why`, plainly, and offer no workaround (a Mac, for one, has no build yet).",
        title = "What I can set up for you",
        annotations(title = "What I can set up for you", read_only_hint = true, open_world_hint = false)
    )]
    async fn workflow_list(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<WorkflowList>, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        let result = self.workflows().map_err(Json).map(|_| {
            let site = self.config.site();
            let ctx = Ctx::new(&site);
            WorkflowList {
                workflows: site
                    .workflows
                    .workflows
                    .iter()
                    .map(|workflow| {
                        let why = self.workflow_unavailable(&site, workflow);
                        WorkflowOffer {
                            id: workflow.id.clone(),
                            title: ctx.plain(&workflow.title),
                            intro: ctx.plain(&workflow.intro),
                            available: why.is_none(),
                            why,
                            steps: workflow
                                .steps
                                .iter()
                                .map(|step| StepOffer {
                                    kind: step.kind.clone(),
                                    title: ctx.plain(&step.title),
                                    os: step.os.clone(),
                                    client: step.client.clone(),
                                })
                                .collect(),
                        }
                    })
                    .collect(),
            }
        });
        self.record_workflow(&subject, "workflow_list", "", result).map(Json)
    }

    #[tool(
        name = "workflow_start",
        description = "Start a workflow from workflow_list for the user and get the one link that does it. The link is all they need: no sign-in, no account to make. Opening it changes nothing; on the page they pick what their machine runs, press Get my command, and paste the one line it shows on that machine; the hub ticks each step as it sees it done, and once something worked it offers to keep their account (optional). Give the user `link`, say in a sentence what will happen (`intro`) and that it works until `expires_at` (30 minutes unless the hub says otherwise), then call workflow_wait to follow along. Treat the link like a password: only for the user. `machine_name` names the machine its machine step adds. Starting the same workflow again replaces its earlier link. A tenant waiting for early access, or at its plan's machine limit, gets that said plainly instead: tell the user. Use this when you cannot run commands on the user's computer; if you can, call machine_add and run its command yourself.",
        title = "Start a setup for the user",
        annotations(
            title = "Start a setup for the user",
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn workflow_start(
        &self,
        Parameters(input): Parameters<StartIn>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<WorkflowStarted>, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        let result = self.start_workflow(&subject, input).await;
        let id = result.as_ref().map(|started| started.id.clone()).unwrap_or_default();
        self.record_workflow(&subject, "workflow_start", &id, result).map(Json)
    }

    #[tool(
        name = "workflow_status",
        description = "Where a workflow you started stands: each step done, current, pending or unavailable (an optional one, keeping the account, is never current), with what the hub sees in `fact` (for example \"nas is online\"). Use it to resume, or when the user says they did something; to follow along, workflow_wait is better. An unavailable step says `why`: tell the user plainly. When state is completed, carry on with what the user asked: a machine it added is in `machines`.",
        title = "Check a setup",
        annotations(title = "Check a setup", read_only_hint = true, open_world_hint = false)
    )]
    async fn workflow_status(
        &self,
        Parameters(input): Parameters<IdIn>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<WorkflowStatus>, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        let result = match self.own_workflow(&subject, &input.id).await {
            Ok(row) => self
                .look_at_workflow(&subject, row, None, now_secs())
                .await
                .map_err(Json),
            Err(error) => Err(error),
        };
        self.record_workflow(&subject, "workflow_status", &input.id, result).map(Json)
    }

    #[tool(
        name = "workflow_wait",
        description = "Wait until a workflow you started changes (a step done, the link expiring) or timeout_secs (1 to 60, default 50) passes, then return where it stands, with changed true when something moved; it returns the moment something does. Use it right after giving the user the link, and again after each change, instead of calling workflow_status repeatedly. A wait that times out returns changed false: wait again, or ask the user how it is going.",
        title = "Wait for the user's next step",
        annotations(title = "Wait for the user's next step", read_only_hint = true, open_world_hint = false)
    )]
    async fn workflow_wait(
        &self,
        Parameters(input): Parameters<WaitIn>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<WorkflowWaited>, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        let result = self.wait_workflow(&subject, &input, &context).await;
        self.record_workflow(&subject, "workflow_wait", &input.id, result).map(Json)
    }

    #[tool(
        name = "workflow_cancel",
        description = "Cancel a workflow you started: its link stops working at once. Use it when the user changes their mind or you started the wrong one.",
        title = "Cancel a setup",
        annotations(
            title = "Cancel a setup",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn workflow_cancel(
        &self,
        Parameters(input): Parameters<IdIn>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<Cancelled>, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        let result = match self.own_workflow(&subject, &input.id).await {
            Ok(row) => {
                let workflows = self.workflows().map_err(Json)?;
                let cancelled = workflows
                    .accounts()
                    .cancel_workflow(&row.tenant, &row.id, now_secs())
                    .await
                    .map_err(Json);
                workflows.changed();
                cancelled.map(|cancelled| Cancelled { cancelled })
            }
            Err(error) => Err(error),
        };
        self.record_workflow(&subject, "workflow_cancel", &input.id, result).map(Json)
    }
}

impl Hub {
    async fn start_workflow(&self, subject: &Subject, input: StartIn) -> Result<WorkflowStarted, Json<BridgeError>> {
        let workflows = self.workflows().map_err(Json)?.clone();
        let Some(tenant) = subject.tenant.clone() else {
            return Err(refused("workflows_unavailable", "this account has no space of its own on this hub"));
        };
        let site = self.config.site();
        let Some(definition) = site.workflows.get(input.workflow.trim()) else {
            return Err(refused(
                "unknown_workflow",
                format!("no workflow {:?} here; workflow_list says which there are", input.workflow),
            ));
        };
        if let Some(why) = self.workflow_unavailable(&site, definition) {
            return Err(refused("workflow_unavailable", why));
        }
        // Early access and the plan's limits, said plainly before anyone is handed a link
        // that could add nothing.
        if let Some(refusal) = self.waiting_refusal(subject) {
            return Err(Json(refusal));
        }
        if definition.steps.iter().any(|step| step.kind == crate::site::workflows::MACHINE)
            && let Some(refusal) = self.machine_room(subject)
        {
            return Err(Json(refusal));
        }
        let machine_name = match input.machine_name.as_deref().map(|name| name.trim().to_ascii_lowercase()) {
            Some(name) if name.is_empty() => None,
            Some(name) => {
                let longest = if self.config.multi_tenant() {
                    crate::tenant_machines::HOSTED_NAME_MAX
                } else {
                    63
                };
                if !crate::config::valid_name(&name) || name.len() > longest {
                    return Err(refused(
                        "invalid_name",
                        format!("a machine name is 1 to {longest} lowercase letters, digits and dashes, starting with a letter or digit"),
                    ));
                }
                Some(name)
            }
            None => None,
        };
        let now = now_secs();
        let (id, token, _) = workflows
            .accounts()
            .start_workflow(
                &tenant,
                &subject.id,
                definition,
                machine_name.as_deref(),
                workflows.link_ttl(),
                crate::link::random::<12>(),
                crate::link::random::<32>(),
                now,
            )
            .await
            .map_err(Json)?;
        if let Some(analytics) = &self.analytics {
            analytics.tenant_event(&tenant, crate::analytics::funnel::WORKFLOW);
        }
        workflows.changed();
        let link = self.workflow_link(&token);
        let row = workflows
            .accounts()
            .workflow(&tenant, &id)
            .await
            .map_err(Json)?
            .ok_or_else(|| refused("account_store", "the workflow just started is not in the store"))?;
        let status = self
            .look_at_workflow(subject, row, Some(link.clone()), now)
            .await
            .map_err(Json)?;
        Ok(WorkflowStarted {
            id,
            link,
            expires_at: status.expires_at,
            title: status.title,
            intro: Ctx::new(&site).plain(&definition.intro),
            steps: status.steps,
            // Already done (a phone that is on already): nothing to hand over.
            next: if status.state == "open" {
                format!(
                    "Give the user `link` (it works for {} minutes, for them alone) and say in a sentence what it does (`intro`): they open it, press one button and paste one line. Then call workflow_wait.",
                    workflows.link_ttl().div_ceil(60)
                )
            } else {
                status.next.clone()
            },
        })
    }

    async fn wait_workflow(
        &self,
        subject: &Subject,
        input: &WaitIn,
        context: &RequestContext<RoleServer>,
    ) -> Result<WorkflowWaited, Json<BridgeError>> {
        let secs = input
            .timeout_secs
            .unwrap_or(WAIT_DEFAULT_SECS)
            .clamp(1, WAIT_MAX_SECS);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        let workflows = self.workflows().map_err(Json)?.clone();
        let first = self.own_workflow(subject, &input.id).await?;
        let started = self
            .look_at_workflow(subject, first, None, now_secs())
            .await
            .map_err(Json)?;
        if started.state != "open" {
            return Ok(WorkflowWaited {
                changed: false,
                status: started,
            });
        }
        loop {
            let woken = workflows.changes().notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Ok(WorkflowWaited {
                    changed: false,
                    status: started,
                });
            }
            tokio::select! {
                biased;
                _ = context.ct.cancelled() => {
                    return Err(refused("request_cancelled", "stopped waiting; the workflow goes on"));
                }
                _ = &mut woken => {}
                _ = tokio::time::sleep(left.min(LOOK_EVERY)) => {}
            }
            // The subject as the hub sees it now (an account secured meanwhile).
            let subject = self.fresh_subject(subject).await;
            let row = self.own_workflow(&subject, &input.id).await?;
            let now = self
                .look_at_workflow(&subject, row, None, now_secs())
                .await
                .map_err(Json)?;
            if moved(&started, &now) {
                return Ok(WorkflowWaited {
                    changed: true,
                    status: now,
                });
            }
        }
    }

    /// `subject` as its account stands now: secured or not.
    pub(crate) async fn fresh_subject(&self, subject: &Subject) -> Subject {
        let Ok(workflows) = self.workflows() else {
            return subject.clone();
        };
        match workflows.accounts().get(&subject.id).await {
            Ok(Some(account)) => Subject {
                provisional_until: workflows.accounts().lapses_at(&account),
                ..subject.clone()
            },
            _ => subject.clone(),
        }
    }
}

/// Whether anything the AI acts on moved between two looks at a workflow.
fn moved(before: &WorkflowStatus, after: &WorkflowStatus) -> bool {
    before.state != after.state
        || before.steps.len() != after.steps.len()
        || before
            .steps
            .iter()
            .zip(&after.steps)
            .any(|(a, b)| a.state != b.state || a.fact != b.fact || a.why != b.why)
}
