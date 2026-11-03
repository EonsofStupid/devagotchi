//! Who is let in, and on which plan (increment 63). A hosted hub (`tenancy.mode multi`)
//! reads both from its site (`site.json`): `early_access.mode` says whether a new tenant is
//! let in at once (`open`) or waits for an operator (`early_access`), and `plans` what each
//! tenant may hold (`site/plans.rs`). Each tenant's row in `tenancy_access` (migration
//! 0008) keeps its state and its plan; the row is the tenant's own, under row-level
//! security like the rest.
//!
//! - A new tenant is `admitted` when sign-up is open and `waiting` (since now) when it is
//!   early access, on the site's default plan. The home tenant, the operator's, is always
//!   in; so is every tenant made before early access existed.
//! - A waiting tenant's account works as before: it is made on first connect, it is
//!   secured with any sign-in, its AI connects and asks `whoami`. Nothing else runs for it:
//!   no tool, no machine added or enrolled, no board written (`hub.rs`,
//!   `tenant_machines.rs`, `lib.rs`, `web/approvals.rs`).
//! - Opening sign-up lets each waiting tenant in on its next request (`settle_in`),
//!   recorded as `open sign-up`. Going back to early access never puts anyone back.
//! - An operator lets tenants in and changes their plans (`admit`, `set_plan`); each step
//!   is recorded in the tenant's chain and in the home tenant's, the operator as its actor.
//! - A tenant just let in hears so by email when one of its sign-ins has a verified address
//!   and the hub can send mail (`on_admitted`); otherwise its account page says it.
//! - A plan id the site no longer lists reads as the site's default plan, and the hub says
//!   so in its log, once per tenant and plan.
//!
//! The operator's lists go through `Store::tenants_in`, which names the tenants in one state
//! and nothing else; each tenant's details are read in a transaction scoped to it.

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use super::*;
use crate::{
    site::{Plan, SignUp, Site},
    store::Lookup,
};

pub const ADMITTED: &str = "admitted";
pub const WAITING: &str = "waiting";
/// How a tenant made while sign-up was open, or waiting when it opened, was let in.
pub const OPEN_SIGN_UP: &str = "open sign-up";
/// How a tenant made before early access existed was let in.
const BEFORE: &str = "before early access";
const HOME: &str = "home tenant";

/// A tenant's place on the hub.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Access {
    /// Waiting for an operator to let it in.
    pub waiting: bool,
    /// When it joined the early-access list.
    pub waiting_since: Option<u64>,
    /// When it was let in, and by whom (an operator's account, or how).
    pub admitted_at: Option<u64>,
    pub admitted_by: Option<String>,
    /// The plan id it is on, as kept; `None`: the site's default plan.
    pub plan: Option<String>,
}

/// A tenant just let in, for the email that says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    pub tenant: String,
    /// The first verified address among its sign-ins.
    pub email: Option<String>,
}

/// What hears that tenants were let in: the hub's mail, when it has a server.
pub type Announce = Arc<dyn Fn(Admitted) + Send + Sync>;

fn access_row(row: &AnyRow) -> Result<(Access, bool), sqlx::Error> {
    Ok((
        Access {
            waiting: row.try_get::<String, _>(0)? == WAITING,
            waiting_since: row.try_get::<Option<i64>, _>(1)?.map(unsecs),
            admitted_at: row.try_get::<Option<i64>, _>(2)?.map(unsecs),
            admitted_by: row.try_get(3)?,
            plan: row.try_get(4)?,
        },
        row.try_get::<i64, _>(5)? == 1,
    ))
}

/// `tenant`'s access as kept, and whether it is the home tenant; `None` for a tenant with
/// no row yet. Inside a transaction scoped to `tenant`.
async fn read_in(
    connection: &mut AnyConnection,
    tenant: &str,
) -> Result<Option<(Access, bool)>, sqlx::Error> {
    sqlx::query(
        "SELECT a.state, a.waiting_since, a.admitted_at, a.admitted_by, a.plan, t.home
         FROM tenancy_access a JOIN tenancy_tenants t ON t.id = a.tenant_id
         WHERE a.tenant_id = $1",
    )
    .bind(tenant)
    .fetch_optional(&mut *connection)
    .await?
    .as_ref()
    .map(access_row)
    .transpose()
}

async fn insert_in(
    connection: &mut AnyConnection,
    tenant: &str,
    access: &Access,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO tenancy_access (tenant_id, state, waiting_since, admitted_at, admitted_by, plan)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(tenant)
    .bind(if access.waiting { WAITING } else { ADMITTED })
    .bind(access.waiting_since.map(secs))
    .bind(access.admitted_at.map(secs))
    .bind(&access.admitted_by)
    .bind(&access.plan)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// A new tenant's row: in, or waiting since `now`, as the site's sign-up is now; on its
/// default plan. Inside the transaction that makes the tenant, scoped to it.
pub(super) async fn begin_in(
    connection: &mut AnyConnection,
    tenant: &str,
    site: &Site,
    now: u64,
) -> Result<Access, Failed> {
    let waiting = site.sign_up() == SignUp::EarlyAccess;
    let access = Access {
        waiting,
        waiting_since: waiting.then_some(now),
        admitted_at: (!waiting).then_some(now),
        admitted_by: (!waiting).then(|| OPEN_SIGN_UP.to_string()),
        plan: site.default_plan().map(|plan| plan.id.clone()),
    };
    insert_in(connection, tenant, &access).await?;
    let (event, detail) = if waiting {
        ("tenant.waiting", "early access")
    } else {
        ("tenant.admitted", OPEN_SIGN_UP)
    };
    tenants::record_in(connection, tenant, "hub", event, detail, now).await?;
    Ok(access)
}

/// The home tenant's row, made if missing: always in. Scoped to the home tenant.
pub(super) async fn home_in(
    connection: &mut AnyConnection,
    home: &str,
    now: u64,
) -> Result<(), sqlx::Error> {
    if read_in(connection, home).await?.is_none() {
        insert_in(
            connection,
            home,
            &Access {
                admitted_at: Some(now),
                admitted_by: Some(HOME.to_string()),
                ..Access::default()
            },
        )
        .await?;
    }
    Ok(())
}

/// The first verified address among `tenant`'s sign-ins. Scoped to `tenant`.
async fn verified_email_in(
    connection: &mut AnyConnection,
    tenant: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT email FROM auth_identities WHERE tenant_id = $1 AND email IS NOT NULL
         ORDER BY created_at, provider, subject LIMIT 1",
    )
    .bind(tenant)
    .fetch_optional(&mut *connection)
    .await
}

impl Accounts {
    /// The site plans and early access are read from, as it is now.
    pub(crate) fn site(&self) -> Arc<Site> {
        self.site.get()
    }

    /// Who hears that tenants were let in (set once, when the hub starts with mail).
    pub fn on_admitted(&self, announce: Announce) {
        let _ = self.announce.set(announce);
    }

    /// Tell whoever listens about the tenants just let in. After their transaction.
    pub(super) fn announce(&self, admitted: impl IntoIterator<Item = Admitted>) {
        if let Some(announce) = self.announce.get() {
            for one in admitted {
                announce(one);
            }
        }
    }

    /// Let `tenant` in, inside a transaction scoped to it that has found it waiting, and
    /// record it in its chain and the home tenant's (the transaction is scoped to `tenant`
    /// again after). `plan`, when given, is the plan it goes on.
    async fn admit_in(
        &self,
        connection: &mut AnyConnection,
        tenant: &str,
        by: &str,
        plan: Option<&str>,
        now: u64,
    ) -> Result<Admitted, Failed> {
        sqlx::query(
            "UPDATE tenancy_access SET state = 'admitted', admitted_at = $2, admitted_by = $3,
               plan = coalesce($4, plan)
             WHERE tenant_id = $1 AND state = 'waiting'",
        )
        .bind(tenant)
        .bind(secs(now))
        .bind(by)
        .bind(plan)
        .execute(&mut *connection)
        .await?;
        let (actor, detail) = match by {
            OPEN_SIGN_UP | HOME => ("hub", by.to_string()),
            operator => (operator, format!("plan {}", plan.unwrap_or("kept"))),
        };
        tenants::record_in(connection, tenant, actor, "tenant.admitted", &detail, now).await?;
        let email = verified_email_in(connection, tenant).await?;
        let home = tenants::home_in(connection).await?;
        if home != tenant {
            self.store.scope(connection, &home).await?;
            tenants::record_in(
                connection,
                &home,
                actor,
                "tenant.admitted",
                &format!("{tenant}: {detail}"),
                now,
            )
            .await?;
            self.store.scope(connection, tenant).await?;
        }
        Ok(Admitted {
            tenant: tenant.to_string(),
            email,
        })
    }

    /// `account`'s tenant's access, settled now, set on `account`: a tenant with no row yet
    /// (made by a hub from before early access) is in; a waiting one is let in now when
    /// sign-up is open, or when it is the home tenant. Inside a transaction scoped to the
    /// account's tenant; the tenant it let in, if it did.
    pub(super) async fn settle_in(
        &self,
        connection: &mut AnyConnection,
        account: &mut Account,
        now: u64,
    ) -> Result<Option<Admitted>, Failed> {
        let tenant = account.tenant.clone();
        if tenant.is_empty() {
            return Ok(None);
        }
        let (mut access, home) = match read_in(connection, &tenant).await? {
            Some(found) => found,
            None => {
                let access = Access {
                    admitted_at: Some(now),
                    admitted_by: Some(BEFORE.to_string()),
                    ..Access::default()
                };
                insert_in(connection, &tenant, &access).await?;
                (access, false)
            }
        };
        let mut admitted = None;
        if access.waiting && (home || self.site().sign_up() == SignUp::Open) {
            let by = if home { HOME } else { OPEN_SIGN_UP };
            admitted = Some(self.admit_in(connection, &tenant, by, None, now).await?);
            access = read_in(connection, &tenant)
                .await?
                .map(|(access, _)| access)
                .unwrap_or_default();
        }
        account.access = Some(access);
        Ok(admitted)
    }

    /// The access of account `id`'s tenant, settled now as a request of its own would
    /// settle it; `None` for an account this hub does not have.
    pub async fn account_access(&self, id: &str) -> BridgeResult<Option<Access>> {
        let now = now_secs();
        let found = transaction!(&self.store, |connection| {
            if self.enter(connection, Lookup::Account(id)).await?.is_none() {
                return Ok(None);
            }
            let Some(mut account) = account_in(connection, id).await? else {
                return Ok(None);
            };
            let admitted = self.settle_in(connection, &mut account, now).await?;
            Ok(Some((account.access, admitted)))
        })?;
        Ok(found.and_then(|(access, admitted)| {
            self.announce(admitted);
            access
        }))
    }

    /// What `/enroll` and the link ask before a machine joins with an account's code: no
    /// while its tenant waits to be let in, or when the store cannot say.
    pub fn enroll_gate(self: &Arc<Self>) -> crate::link::EnrollGate {
        let accounts = self.clone();
        Arc::new(move |owner: String| {
            let accounts = accounts.clone();
            Box::pin(async move {
                matches!(
                    accounts.account_access(&owner).await,
                    Ok(None) | Ok(Some(Access { waiting: false, .. }))
                )
            })
        })
    }

    /// `tenant`'s access as kept, or `None` for a tenant this hub does not have.
    #[cfg_attr(not(test), allow(dead_code))] // the operator page (increment 64)
    pub async fn access_of(&self, tenant: &str) -> BridgeResult<Option<Access>> {
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            Ok(read_in(connection, tenant).await?.map(|(access, _)| access))
        })
    }

    /// The tenants in `state` ([`ADMITTED`] or [`WAITING`]), at most `most`: the longest
    /// waiting first, the latest let in first.
    #[cfg_attr(not(test), allow(dead_code))] // the operator page (increment 64)
    pub async fn tenants_in(&self, state: &str, most: u32) -> BridgeResult<Vec<String>> {
        transaction!(&self.store, |connection| {
            Ok(self
                .store
                .tenants_in(connection, state, i64::from(most))
                .await?)
        })
    }

    /// An operator lets `tenants` in, each on `plan` when given (a plan the site lists).
    /// Tenants already in are left as they are. The operator's account is the actor in each
    /// tenant's chain and in the home tenant's; whoever listens hears of each one let in.
    #[cfg_attr(not(test), allow(dead_code))] // the operator page (increment 64)
    pub async fn admit(
        &self,
        tenants: &[String],
        plan: Option<&str>,
        operator: &str,
    ) -> BridgeResult<Vec<Admitted>> {
        if let Some(plan) = plan {
            self.known_plan(plan)?;
        }
        let now = now_secs();
        let admitted = transaction!(&self.store, |connection| {
            let mut admitted = Vec::new();
            for tenant in tenants {
                self.store.scope(connection, tenant).await?;
                match read_in(connection, tenant).await? {
                    Some((access, _)) if access.waiting => admitted.push(
                        self.admit_in(connection, tenant, operator, plan, now)
                            .await?,
                    ),
                    Some(_) => {}
                    None => {
                        return Err(BridgeError::new(
                            "unknown_tenant",
                            format!("no tenant {tenant} here"),
                        )
                        .into());
                    }
                }
            }
            Ok(admitted)
        })?;
        self.announce(admitted.clone());
        Ok(admitted)
    }

    /// An operator puts `tenant` on `plan` (a plan the site lists), recorded in its chain and
    /// the home tenant's.
    #[cfg_attr(not(test), allow(dead_code))] // the operator page (increment 64)
    pub async fn set_plan(&self, tenant: &str, plan: &str, operator: &str) -> BridgeResult<()> {
        self.known_plan(plan)?;
        let now = now_secs();
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            let changed = sqlx::query("UPDATE tenancy_access SET plan = $2 WHERE tenant_id = $1")
                .bind(tenant)
                .bind(plan)
                .execute(&mut *connection)
                .await?
                .rows_affected();
            if changed == 0 {
                return Err(
                    BridgeError::new("unknown_tenant", format!("no tenant {tenant} here")).into(),
                );
            }
            tenants::record_in(connection, tenant, operator, "tenant.plan", plan, now).await?;
            let home = tenants::home_in(connection).await?;
            self.store.scope(connection, &home).await?;
            tenants::record_in(
                connection,
                &home,
                operator,
                "tenant.plan",
                &format!("{tenant}: {plan}"),
                now,
            )
            .await
        })
    }

    /// Refuse a new AI connection for `tenant` once it has as many live ones (each refresh
    /// family is one) as its plan's `clients`. Inside a transaction scoped to it.
    pub(super) async fn room_on_the_plan(
        &self,
        connection: &mut AnyConnection,
        tenant: &str,
        now: u64,
    ) -> Result<(), Failed> {
        let Some((access, _)) = read_in(connection, tenant).await? else {
            return Ok(());
        };
        let Some(plan) = self.plan_of(tenant, &access) else {
            return Ok(());
        };
        let crate::site::Cap::AtMost(most) = plan.cap("clients") else {
            return Ok(());
        };
        let live: i64 = sqlx::query(
            "SELECT COUNT(DISTINCT family) FROM oauth_tokens
             WHERE tenant_id = $1 AND kind = 'refresh' AND used_at IS NULL AND expires_at > $2",
        )
        .bind(tenant)
        .bind(secs(now))
        .fetch_one(&mut *connection)
        .await?
        .try_get(0)?;
        let live = usize::try_from(live).unwrap_or(0);
        if live >= most as usize {
            let site = self.site();
            return Err(BridgeError::new(
                "plan_limit",
                site.limit_message(&plan, "clients", most, live, &self.public_url),
            )
            .into());
        }
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))] // the operator page (increment 64)
    fn known_plan(&self, plan: &str) -> BridgeResult<()> {
        if self.site().plan(plan).is_none() {
            return Err(BridgeError::new(
                "unknown_plan",
                format!("site.json lists no plan {plan:?}"),
            ));
        }
        Ok(())
    }

    /// The plan `tenant`, kept on `access`, is on as the site is now: its plan, else the
    /// site's default plan (said in the log once per tenant and plan id).
    pub fn plan_of(&self, tenant: &str, access: &Access) -> Option<Plan> {
        plan_for(&self.site(), tenant, access)
    }
}

/// The plan a tenant kept on `access` is on in `site`; a plan id the site no longer lists
/// is its default plan, said in the log once per tenant and plan id.
pub fn plan_for(site: &Site, tenant: &str, access: &Access) -> Option<Plan> {
    static SAID: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());
    let (plan, fell_back) = site.plan_for(access.plan.as_deref());
    if fell_back {
        let kept = access.plan.clone().unwrap_or_default();
        let first = SAID
            .lock()
            .map(|mut said| said.insert((tenant.to_string(), kept.clone())))
            .unwrap_or(false);
        if first {
            eprintln!(
                "devagotchi hub: tenant {tenant} is on plan {kept:?}, which site.json no longer lists; holding it to the default plan {:?}",
                plan.map(|plan| plan.id.as_str()).unwrap_or("")
            );
        }
    }
    plan.cloned()
}

#[cfg(test)]
mod tests;
