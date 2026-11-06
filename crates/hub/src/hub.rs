//! The hub's MCP surface: the broker's four handle-based tools, each routed to a
//! named broker the authenticated subject holds a grant for, plus `whoami` and `machines`
//! (what this subject can reach and whether each machine can be used right now).

use std::{path::PathBuf, sync::Arc};

use devagotchi_contracts::{
    BridgeError, GetInput, OperationView, OutputPage, ReadInput, RequestBody, ResponseBody,
    StartInput, StartOutput, WaitInput,
};
use rmcp::{
    Json, RoleServer, ServerHandler, handler::server::wrapper::Parameters, service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    audit::{Audit, AuditEntry},
    auth::Subject,
    board::mcp::Boards,
    config::HubConfig,
    link::Links,
};

#[derive(Clone)]
pub struct Hub {
    pub config: Arc<HubConfig>,
    pub audit: Arc<Audit>,
    pub links: Arc<Links>,
    /// The project boards this hub serves (`board_*` tools, `crate::board::mcp`).
    pub boards: Arc<Boards>,
    /// Web Push to people's phones when an approval waits; present with browser sign-in.
    pub push: Option<Arc<crate::push::Push>>,
    /// The build this hub hands to machines that join it (`/join`, `/dist`), when configured.
    pub downloads: Option<Arc<crate::downloads::Downloads>>,
    /// What a hosted hub counts for its operator (`crate::analytics`), when it counts.
    pub analytics: Option<Arc<crate::analytics::Analytics>>,
    /// The workflows an AI starts for its person (`crate::workflows`), on a hub with its own
    /// accounts.
    pub workflows: Option<Arc<crate::workflows::Workflows>>,
    /// The site's offers and how an account is linked (`crate::offers`), on a hub with its
    /// own accounts.
    pub offers: Option<Arc<crate::offers::Offers>>,
}

/// One machine as a person sees it on their page and in `/api/v1/me`, and as an AI sees it
/// through the `machines` tool. The hub is its one owner.
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct Machine {
    /// The name to pass as `broker` to the operation tools.
    pub name: String,
    /// `local`: a broker on the hub's own host. `linked`: a machine that dials in.
    pub kind: &'static str,
    /// `online`, `offline` (off, asleep, or its link is not running), `not_enrolled` (a linked
    /// machine that never enrolled) or `revoked`.
    pub status: &'static str,
    /// When the machine enrolled, in seconds since the Unix epoch (linked machines).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrolled_at: Option<u64>,
    /// The account added this machine itself ("Add a machine"); only it reaches the
    /// machine, and it can revoke it. Left out when false, so `default` keeps it out of the
    /// output schema's `required` (a client that checks refuses `machines` otherwise).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owned: bool,
    /// The build the machine runs, as its link said when it connected (online linked
    /// machines whose link says it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// `updating` while an update to the hub's build runs on it, or `failed: <reason>, back
    /// on <build>` after one failed. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Machines {
    /// Every machine this identity can work on, each with whether it can be used now.
    pub machines: Vec<Machine>,
}

/// Where a granted broker lives: on this host, or behind a machine's link.
pub(crate) enum Route {
    Local(PathBuf),
    Link(String),
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartRouted {
    /// Name of the enrolled broker that owns the target machine.
    pub broker: String,
    pub input: StartInput,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetRouted {
    pub broker: String,
    pub input: GetInput,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitRouted {
    pub broker: String,
    pub input: WaitInput,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadRouted {
    pub broker: String,
    pub input: ReadInput,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MachineAdd {
    /// Short name for the machine, `[a-z0-9][a-z0-9-]{0,62}`.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MachineUpdate {
    /// The machine's name, as `machines` lists it.
    pub machine: String,
    /// Update now even while the machine runs jobs; they end when its broker restarts.
    #[serde(default)]
    pub force: bool,
    /// How long to wait for the machine to come back, in seconds (default 50, at most 300).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MachineUpdated {
    pub machine: String,
    /// `done`, `failed`, or where it still is when the wait ended: `requested`,
    /// `waiting_for_jobs`, `downloading` or `installing`.
    pub state: String,
    /// Why it failed, or `already on <build>` when there was nothing to do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The build the machine runs now, while it is online.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MachineAdded {
    pub name: String,
    pub code: String,
    pub expires_at: u64,
    pub linux: String,
    pub windows: String,
    /// Compose file URL. The code is the same single-use join token.
    pub docker: String,
    /// Host installer for Proxmox or Incus (not inside a guest).
    pub host: String,
    /// Tarball plus `devagotchi link enroll`, code on stdin.
    pub manual: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WhoAmI {
    /// The account ID (`sub`) the identity provider proved, never a caller-supplied name.
    /// Grants are written against this.
    pub subject: String,
    pub via: String,
    /// The login name the identity provider reported, to tell your accounts apart.
    /// Grants never use it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The org (tenant) the identity provider proved this account belongs to. Projects
    /// bound by any member belong to it and every member works them (B13).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    /// Unix seconds until an agent-opened account must be claimed. Absent once claimed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provisional_until: Option<u64>,
    /// The agent seat on this token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Code the person uses to claim this account. Shown while it is provisional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_code: Option<String>,
    /// Time left is under a seventh of the live provisional ttl (at least an hour).
    /// `default` keeps it out of the schema's `required`, since it is left out when false:
    /// clients check `structuredContent` against the output schema (the MCP TypeScript SDK
    /// refuses the call otherwise).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provisional_warning: bool,
    /// `{public_url}/claim?code=…` while the account is provisional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_url: Option<String>,
    /// The tenant this account belongs to on a hub with its own accounts (`t-<uuid>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// On a hub with its own accounts: `admitted`, or `waiting` while the tenant is on the
    /// early-access list. While it waits, every other tool answers with `access_note`
    /// instead of acting: tell the person, once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    /// When the tenant joined the early-access list, in Unix seconds, while it waits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_since: Option<u64>,
    /// What the hub says to a waiting tenant: when it joined, that an email comes when it
    /// is in, and its account page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_note: Option<String>,
    /// The plan the tenant is on (a plan id from the site), on a hosted hub.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Where its person secures this account, while nobody has: until then it is bound to
    /// the browser that made it and the AI apps it approved, nothing can recover it, and it
    /// ends at `provisional_until` if unused. Tell the person, and give them this link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure_url: Option<String>,
    /// Whether this is the hub's configured owner, who reaches every broker and board.
    pub owner: bool,
    /// Whether this subject acts as a person on boards (force-release, moving tickets by
    /// hand): the owner, or a grant marked human. Agents are never people.
    pub human: bool,
    /// Brokers this subject holds a grant for, by name.
    pub brokers: Vec<String>,
    /// Project boards this subject may work on, by project name: its org's (or, with no
    /// org, the account's own; `board_bind`) first, then the shared ones it holds a grant for.
    pub boards: Vec<String>,
}

impl Hub {
    /// The machines `subject` reaches, each with whether it can be used right now, named
    /// as it knows them (on a hosted hub, its tenant's names: `tenant_machines.rs`).
    pub async fn machine_status(&self, subject: &Subject) -> Vec<Machine> {
        let configured = self.config.brokers_for(&subject.id);
        let owned: Vec<Machine> = self
            .owned(&subject.id)
            .into_iter()
            .filter(|device| configured.iter().all(|b| b.name != device.name))
            .map(|device| Machine {
                status: if device.revoked_at.is_some() {
                    "revoked"
                } else if self.links.connected(&device.name) {
                    "online"
                } else {
                    "offline"
                },
                name: self.shown_name(subject, &device.name),
                kind: "linked",
                enrolled_at: Some(device.enrolled_at),
                owned: true,
                build: self.links.build(&device.name),
                update: self.update_shown(&device.name),
            })
            .collect();
        let checks = configured.into_iter().map(|broker| async move {
            match &broker.socket {
                Some(socket) => Machine {
                    name: broker.name.clone(),
                    kind: "local",
                    status: if crate::reachable(socket).await {
                        "online"
                    } else {
                        "offline"
                    },
                    enrolled_at: None,
                    owned: false,
                    build: None,
                    update: None,
                },
                None => {
                    let (status, enrolled_at) = match self.links.device(&broker.name) {
                        Ok(Some(device)) if device.revoked_at.is_some() => {
                            ("revoked", Some(device.enrolled_at))
                        }
                        Ok(Some(device)) if self.links.connected(&broker.name) => {
                            ("online", Some(device.enrolled_at))
                        }
                        Ok(Some(device)) => ("offline", Some(device.enrolled_at)),
                        Ok(None) => ("not_enrolled", None),
                        Err(_) => ("offline", None),
                    };
                    Machine {
                        name: broker.name.clone(),
                        kind: "linked",
                        status,
                        enrolled_at,
                        owned: false,
                        build: self.links.build(&broker.name),
                        update: self.update_shown(&broker.name),
                    }
                }
            }
        });
        let mut machines = futures_util::future::join_all(checks).await;
        machines.extend(owned);
        machines
    }

    /// What `machines` says about `device`'s latest update (`machine_update.rs`).
    fn update_shown(&self, device: &str) -> Option<String> {
        let build = self.links.build(device);
        self.links
            .update_of(device)
            .and_then(|update| update.shown(build.as_deref(), crate::devices::now_secs()))
    }

    /// The machines `subject` added with codes of its own (increment 29a). A store that
    /// cannot be read lists none rather than failing the caller's whole request.
    pub(crate) fn owned(&self, subject: &str) -> Vec<crate::devices::Device> {
        self.links.owned_by(subject).unwrap_or_default()
    }

    pub(crate) fn subject(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Subject, Json<BridgeError>> {
        context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<Subject>())
            .cloned()
            .ok_or_else(|| {
                Json(BridgeError::new(
                    "unauthenticated",
                    "no authenticated subject on this request",
                ))
            })
    }

    pub(crate) fn route(
        &self,
        subject: &Subject,
        broker: &str,
    ) -> Result<Route, Json<BridgeError>> {
        let allowed = self.config.brokers_for(&subject.id);
        let unknown = || {
            Err(Json(BridgeError::new(
                "unknown_broker",
                format!("no broker named {broker:?} is enrolled"),
            )))
        };
        match allowed.iter().find(|b| b.name == broker) {
            Some(entry) => Ok(match &entry.socket {
                Some(socket) => Route::Local(PathBuf::from(socket)),
                None => Route::Link(entry.name.clone()),
            }),
            // A machine the caller added is the caller's alone; anyone else is told it
            // does not exist, so names of other accounts' machines are not revealed.
            None if self.config.broker(broker).is_none() => match self.own_machine(subject, broker) {
                Some(device) => Ok(Route::Link(device.name)),
                None => unknown(),
            },
            None => Err(Json(BridgeError::new(
                "not_granted",
                format!("{} holds no grant for broker {broker:?}", subject.id),
            ))),
        }
    }

    /// One broker request along `route`, unaudited: the caller records the call.
    pub(crate) async fn send(
        &self,
        route: &Route,
        body: RequestBody,
    ) -> Result<ResponseBody, BridgeError> {
        match route {
            Route::Local(socket) => devagotchi_ipc::request(socket, body).await,
            Route::Link(device) => self.links.call(device, body).await,
        }
    }

    /// Record one call in the audit chain. A call that cannot be audited is not a call
    /// this bastion made, so a failure here fails the call. An operation that started
    /// (`operation_start`, or `run`) is also its tenant's operation milestone
    /// (`analytics::funnel`): the first, and the latest a day at a time.
    pub(crate) fn record(
        &self,
        subject: &Subject,
        tool: &str,
        broker: &str,
        operation_id: &str,
        outcome: &str,
    ) -> Result<(), Json<BridgeError>> {
        if let (Some(analytics), Some(tenant)) = (&self.analytics, &subject.tenant)
            && matches!(
                (tool, outcome),
                ("operation_start", "ok") | ("run", "ok") | ("run", "running")
            )
        {
            analytics.tenant_event(tenant, crate::analytics::funnel::OPERATION);
        }
        self.audit
            .record(&AuditEntry {
                subject: &subject.id,
                via: subject.via,
                tool,
                broker,
                operation_id,
                outcome,
            })
            .map(|_| ())
            .map_err(Json)
    }

    pub(crate) async fn call(
        &self,
        tool: &str,
        broker: &str,
        body: RequestBody,
        context: RequestContext<RoleServer>,
    ) -> Result<ResponseBody, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        let operation_id = match &body {
            RequestBody::OperationStart(input) => input.idempotency_key.clone(),
            RequestBody::OperationGet(input) | RequestBody::OperationCancel(input) => {
                input.operation_id.clone()
            }
            RequestBody::OperationRead(input) => input.operation_id.clone(),
            RequestBody::OperationWait(input) => input.operation_id.clone(),
            RequestBody::FileRead(input) => input.path.clone(),
            RequestBody::FileList(input) => input.path.clone(),
            RequestBody::FileWrite(input) => input.path.clone(),
            RequestBody::FileEdit(input) => input.path.clone(),
            RequestBody::FileInfo(input) => input.path.clone(),
            RequestBody::FileMkdir(input) => input.path.clone(),
            RequestBody::FileMove(input) => format!("{} -> {}", input.source, input.destination),
            _ => String::new(),
        };
        let result = match self.route(&subject, broker) {
            Ok(route) => {
                tokio::select! {
                    biased;
                    _ = context.ct.cancelled() => Err(Json(BridgeError::new(
                        "request_cancelled",
                        "stopped waiting for the broker response; admitted operations are not cancelled",
                    ))),
                    response = self.send(&route, body) => response.map_err(Json),
                }
            }
            Err(error) => Err(error),
        };
        let outcome = match &result {
            Ok(_) => "ok".to_string(),
            Err(Json(error)) => error.code.clone(),
        };
        let recorded_id = match &result {
            Ok(ResponseBody::Start(output)) => output.operation.operation_id.clone(),
            _ => operation_id,
        };
        self.record(&subject, tool, broker, &recorded_id, &outcome)?;
        result
    }
}

#[tool_router]
impl Hub {
    #[tool(
        name = "whoami",
        description = "The subject this hub authenticated for the current request and the brokers it may route to. Identity comes from the bearer token, never from arguments.",
        title = "Who am I",
        annotations(title = "Who am I", read_only_hint = true, open_world_hint = false)
    )]
    async fn whoami(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<WhoAmI>, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        let access_note = subject.waiting().then(|| self.waiting_text(&subject));
        let plan = self.held_plan(&subject).map(|plan| plan.id);
        Ok(Json(WhoAmI {
            access: subject.access.as_ref().map(|access| {
                if access.waiting {
                    crate::accounts::access::WAITING
                } else {
                    crate::accounts::access::ADMITTED
                }
                .to_string()
            }),
            waiting_since: subject
                .access
                .as_ref()
                .filter(|access| access.waiting)
                .and_then(|access| access.waiting_since),
            access_note,
            plan,
            brokers: {
                let mut brokers: Vec<String> = self
                    .config
                    .brokers_for(&subject.id)
                    .into_iter()
                    .map(|b| b.name.clone())
                    .collect();
                for device in self.own_machines(&subject) {
                    if !brokers.contains(&device.name) {
                        brokers.push(device.name);
                    }
                }
                brokers
            },
            boards: {
                let mut boards = self
                    .boards
                    .account_boards(&crate::board::mcp::Boards::owner_of(&subject));
                boards.extend(
                    self.config
                        .boards_for(&subject.id)
                        .into_iter()
                        .map(|b| b.project.clone()),
                );
                boards
            },
            owner: self.config.is_owner(&subject.id),
            // An AI connected through the hub's own tokens never has a person's standing.
            human: subject.via != "hub" && self.config.is_human(&subject.id),
            subject: subject.id,
            via: subject.via.to_string(),
            name: subject.name,
            org: subject.org,
            provisional_until: subject.provisional_until,
            agent: subject.agent.clone(),
            claim_code: subject.claim_code.clone(),
            provisional_warning: subject.provisional_warning,
            secure_url: (subject.via == "hub" || subject.via == "passkey")
                .then_some(subject.provisional_until)
                .flatten()
                .map(|_| {
                    format!(
                        "{}/account#secure",
                        self.config.public_url.trim_end_matches('/')
                    )
                }),
            tenant: subject.tenant.clone(),
            claim_url: subject.claim_code.as_ref().map(|code| {
                format!(
                    "{}/claim?code={code}",
                    self.config.public_url.trim_end_matches('/')
                )
            }),
        }))
    }

    #[tool(
        name = "machine_add",
        description = "Add one of the user's own machines to this account. Returns a one-time code and every install command: Linux (systemd), Windows (no administrator), Docker Compose, a Proxmox or Incus host, and a manual tarball. The code works once. The machine that enrolls belongs to this account. Do not use a name the hub operator already configured. If you can run commands on this computer, run the command for its OS yourself; give the user a command only for another machine.",
        title = "Add a machine",
        annotations(
            title = "Add a machine",
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn machine_add(
        &self,
        Parameters(input): Parameters<MachineAdd>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<MachineAdded>, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        let name = input.name.trim().to_ascii_lowercase();
        let (device, code) = self.add_own_machine(&subject, &name).map_err(Json)?;
        let hub = self.config.public_url.trim_end_matches('/');
        Ok(Json(MachineAdded {
            name,
            code: code.clone(),
            expires_at: crate::devices::now_secs() + crate::devices::CODE_TTL_SECS,
            linux: join_line(hub, "linux", &code),
            windows: join_line(hub, "windows", &code),
            docker: join_line(hub, "docker", &code),
            host: join_line(hub, "host", &code),
            manual: format!(
                "Download the archive and SHA256SUMS from {hub}/dist/ for this machine, check the sum, unpack, then: printf '%s\\n' '{code}' | devagotchi link enroll --state-dir ~/.local/state/devagotchi-link --hub {hub} --device {device} --broker <broker-endpoint>"
            ),
        }))
    }

    #[tool(
        name = "machine_update",
        description = "Update one of the user's machines to the hub's current build now. Waits for its running jobs to finish unless force is true; the machine is offline for about a minute.",
        title = "Update a machine",
        annotations(
            title = "Update a machine",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn machine_update(
        &self,
        Parameters(input): Parameters<MachineUpdate>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<MachineUpdated>, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        let device = match self.route(&subject, &input.machine)? {
            Route::Link(device) => device,
            Route::Local(_) => {
                return Err(Json(BridgeError::new(
                    "update_unsupported",
                    format!(
                        "{} is a broker on the hub's own host; it updates with the hub",
                        input.machine
                    ),
                )));
            }
        };
        let asked = self
            .links
            .request_update(&device, input.force, &subject.id, subject.via)
            .map_err(Json)?;
        let (state, detail) = match asked {
            crate::link::UpdateAsked::AlreadyOn(build) => {
                ("done".to_string(), Some(format!("already on {build}")))
            }
            crate::link::UpdateAsked::Sent => {
                let within =
                    std::time::Duration::from_secs(input.wait_seconds.unwrap_or(50).min(300));
                match self.links.wait_update(&device, within).await {
                    Some(update) => (update.state, update.detail),
                    None => ("requested".to_string(), None),
                }
            }
        };
        Ok(Json(MachineUpdated {
            machine: input.machine,
            state,
            detail,
            build: self.links.build(&device),
        }))
    }

    #[tool(
        name = "machines",
        description = "Start here for anything on the user's own machines (servers, PCs, devspaces): every machine you can work on through this hub and whether it can be used right now. status is online, offline (the machine is off, asleep or its link is not running: tell the user rather than retrying), not_enrolled or revoked. Pass a machine's name as `broker` to operation_start, operation_get, operation_read and operation_cancel.",
        title = "List my machines",
        annotations(
            title = "List my machines",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    async fn machines(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<Machines>, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        Ok(Json(Machines {
            machines: self.machine_status(&subject).await,
        }))
    }

    #[tool(
        name = "operation_start",
        description = "Start a broker-owned operation on the named broker: a `command` line for the machine's shell (bash or sh on Linux, PowerShell on Windows) on a machine whose broker runs commands as its account, or an `executable` with exact `args` (on other machines it must be operator-granted). The broker validates the deadline and idempotency key; reuse the same key and inputs to reconcile a lost response.",
        title = "Run a program on a machine",
        annotations(
            title = "Run a program on a machine",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn operation_start(
        &self,
        Parameters(routed): Parameters<StartRouted>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<StartOutput>, Json<BridgeError>> {
        match self
            .call(
                "operation_start",
                &routed.broker,
                RequestBody::OperationStart(routed.input),
                context,
            )
            .await?
        {
            ResponseBody::Start(output) => Ok(Json(output)),
            other => Err(unexpected_response("start", &other)),
        }
    }

    #[tool(
        name = "operation_get",
        description = "Read the named broker's retained state for an operation. This does not start, resume or cancel execution.",
        title = "Check an operation",
        annotations(
            title = "Check an operation",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    async fn operation_get(
        &self,
        Parameters(routed): Parameters<GetRouted>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<OperationView>, Json<BridgeError>> {
        match self
            .call(
                "operation_get",
                &routed.broker,
                RequestBody::OperationGet(routed.input),
                context,
            )
            .await?
        {
            ResponseBody::Operation(output) => Ok(Json(output)),
            other => Err(unexpected_response("operation", &other)),
        }
    }

    #[tool(
        name = "operation_wait",
        description = "Wait on the named broker until an operation finishes, or (with output_after, your read cursor) until it has new output, or until timeout_ms (at most 50000) passes. The broker answers the moment it changes; use this instead of calling operation_get or operation_read repeatedly. A wait that times out returns the operation unchanged: wait again.",
        title = "Wait for an operation",
        annotations(
            title = "Wait for an operation",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    async fn operation_wait(
        &self,
        Parameters(routed): Parameters<WaitRouted>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<OperationView>, Json<BridgeError>> {
        match self
            .call(
                "operation_wait",
                &routed.broker,
                RequestBody::OperationWait(routed.input),
                context,
            )
            .await?
        {
            ResponseBody::Operation(output) => Ok(Json(output)),
            other => Err(unexpected_response("operation", &other)),
        }
    }

    #[tool(
        name = "operation_read",
        description = "Read bounded retained output from the named broker after a caller-owned sequence cursor. Lossless base64 bytes, a text view, retention gaps and the operation state.",
        title = "Read an operation's output",
        annotations(
            title = "Read an operation's output",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    async fn operation_read(
        &self,
        Parameters(routed): Parameters<ReadRouted>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<OutputPage>, Json<BridgeError>> {
        match self
            .call(
                "operation_read",
                &routed.broker,
                RequestBody::OperationRead(routed.input),
                context,
            )
            .await?
        {
            ResponseBody::Output(output) => Ok(Json(output)),
            other => Err(unexpected_response("output", &other)),
        }
    }

    #[tool(
        name = "operation_cancel",
        description = "Explicitly request cancellation of a broker-owned operation on the named broker. Disconnecting never cancels.",
        title = "Cancel an operation",
        annotations(
            title = "Cancel an operation",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn operation_cancel(
        &self,
        Parameters(routed): Parameters<GetRouted>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<OperationView>, Json<BridgeError>> {
        match self
            .call(
                "operation_cancel",
                &routed.broker,
                RequestBody::OperationCancel(routed.input),
                context,
            )
            .await?
        {
            ResponseBody::Operation(output) => Ok(Json(output)),
            other => Err(unexpected_response("operation", &other)),
        }
    }
}

/// The one line that joins a machine with `code`, for `os` (`linux`, `windows`, `docker`
/// or `host`): what `machine_add` hands an AI and a workflow's page hands its person.
pub(crate) fn join_line(hub: &str, os: &str, code: &str) -> String {
    let hub = hub.trim_end_matches('/');
    match os {
        "windows" => format!("irm {hub}/join/{code}.ps1 | iex"),
        "docker" => format!(
            "curl -fsSL {hub}/join/{code}.compose.yml -o compose.yml && docker compose -f compose.yml up -d"
        ),
        "host" => format!("curl -fsSL {hub}/join/{code}.host.sh | sudo sh"),
        _ => format!("curl -fsSL {hub}/join/{code} | sudo sh"),
    }
}

pub(crate) fn unexpected_response(expected: &str, actual: &ResponseBody) -> Json<BridgeError> {
    Json(devagotchi_mcp::unexpected(expected, actual))
}

#[tool_handler(
    router = (Self::tool_router() + Self::run_router() + Self::files_router() + Self::board_router() + Self::workflow_router() + Self::offers_router())
)]
impl ServerHandler for Hub {
    /// Every tool call, unless the caller's tenant waits to be let in (early access): then
    /// every tool but `whoami` answers with the site's message instead of acting
    /// (`tenant_access.rs`), and the call is audited as `early_access`.
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        // The offers' tools exist only while the site's offers show (`crate::offers`).
        if !self.offered() && crate::offers::TOOLS.contains(&request.name.as_ref()) {
            return Err(rmcp::ErrorData::invalid_params("tool not found", None));
        }
        let subject = self.subject(&context).ok();
        if request.name != "whoami"
            && let Some(subject) = &subject
            && let Some(answer) = self.waiting_result(subject)
        {
            self.record(subject, &request.name, "", "", "early_access")
                .map_err(|Json(error)| rmcp::ErrorData::internal_error(error.message, None))?;
            return Ok(answer.into());
        }
        let call = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let answer = (Self::tool_router() + Self::run_router() + Self::files_router() + Self::board_router() + Self::workflow_router() + Self::offers_router())
            .call(call)
            .await;
        // A successful call from an AI client of a known family: what a workflow's
        // `ai_client` step waits on (`workflows.rs`).
        if let Ok(rmcp::model::CallToolResponse::Complete(result)) = &answer
            && result.is_error != Some(true)
            && let Some(subject) = &subject
            && let (Some(tenant), Some(client)) = (&subject.tenant, &subject.client)
            && let Some(workflows) = &self.workflows
        {
            workflows.client_called(tenant, client).await;
        }
        answer
    }

    /// Every tool, but the offers' while the site's offers do not show.
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= rmcp::model::ProtocolVersion::V_2026_07_28);
        let offered = self.offered();
        let mut listed = rmcp::model::ListToolsResult::with_all_items(
            (Self::tool_router() + Self::run_router() + Self::files_router() + Self::board_router() + Self::workflow_router() + Self::offers_router())
                .list_all()
                .into_iter()
                .filter(|tool| offered || !crate::offers::TOOLS.contains(&tool.name.as_ref()))
                .collect(),
        );
        listed.ttl_ms = supports_cache_hints.then_some(0);
        listed.cache_scope = supports_cache_hints.then_some(rmcp::model::CacheScope::Public);
        Ok(listed)
    }

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        if !self.offered() && crate::offers::TOOLS.contains(&name) {
            return None;
        }
        (Self::tool_router() + Self::run_router() + Self::files_router() + Self::board_router() + Self::workflow_router() + Self::offers_router())
            .get(name)
            .cloned()
    }

    /// The MCP server's name and instructions are the site's (`site.json` `mcp`), so every
    /// AI client meets the hub under the name the site gives it.
    fn get_info(&self) -> rmcp::model::ServerConfig {
        let site = self.config.site();
        rmcp::model::ServerConfig::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(site.mcp_info())
        .with_instructions(match site.offers_instructions().filter(|_| self.offered()) {
            Some(offers) => format!("{} {offers}", site.mcp_instructions(&self.config.public_url)),
            None => site.mcp_instructions(&self.config.public_url),
        })
    }
}

/// Every tool the hub serves, as `tools/list` describes it.
#[cfg(test)]
pub(crate) fn tools() -> Vec<rmcp::model::Tool> {
    (Hub::tool_router() + Hub::run_router() + Hub::files_router() + Hub::board_router() + Hub::workflow_router() + Hub::offers_router())
        .list_all()
}

/// The names of every tool the hub serves, for tests that hold a page's list to them.
#[cfg(test)]
pub(crate) fn tool_names() -> Vec<String> {
    tools()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    /// What `whoami` writes for an account with nothing optional set must satisfy the output
    /// schema it advertises: every `required` property present (tests/clients_oauth.py found
    /// one missing through the MCP TypeScript SDK, which checks).
    #[test]
    fn whoami_output_has_every_required_property() {
        let tool =
            (Hub::tool_router() + Hub::run_router() + Hub::files_router() + Hub::board_router() + Hub::workflow_router() + Hub::offers_router())
                .list_all()
                .into_iter()
                .find(|tool| tool.name == "whoami")
                .expect("whoami is served");
        let schema = tool.output_schema.expect("whoami has an output schema");
        let written = serde_json::to_value(WhoAmI {
            subject: "300000000000000001".into(),
            via: "hub".into(),
            name: None,
            org: None,
            provisional_until: None,
            agent: None,
            claim_code: None,
            provisional_warning: false,
            claim_url: None,
            tenant: None,
            access: None,
            waiting_since: None,
            access_note: None,
            plan: None,
            secure_url: None,
            owner: false,
            human: false,
            brokers: Vec::new(),
            boards: Vec::new(),
        })
        .unwrap();
        let required = schema["required"].as_array().expect("required properties");
        for property in required {
            let property = property.as_str().unwrap();
            assert!(
                written.get(property).is_some(),
                "whoami's schema requires {property}, which it leaves out"
            );
        }
        assert!(!required.iter().any(|p| p == "provisional_warning"));
    }
}
