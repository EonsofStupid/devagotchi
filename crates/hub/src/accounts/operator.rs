//! What the operator page reads about each tenant (increment 64, `web/operator.rs`): who is
//! waiting to be let in and who is in, with what an operator needs to decide.
//!
//! Which tenants come from `Store::tenants_in` (their ids in one access state, nothing
//! else); each one is then read in the same transaction scoped to that tenant, as its own
//! requests are, so on Postgres row-level security holds every read to one tenant's rows.
//! The home tenant, the operator's own, is left out.

use super::{
    access::{self, Access},
    identities::Linked,
    *,
};

/// One tenant as the operator page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantView {
    pub tenant: String,
    pub access: Access,
    /// Its accounts' ids (on a hosted hub, one).
    pub accounts: Vec<String>,
    /// Whether one of its accounts is secured (a passkey or a linked sign-in).
    pub secured: bool,
    /// The sign-ins linked to it: provider, and the address when it is verified.
    pub sign_ins: Vec<Linked>,
    /// The AI app (or `devagotchi login`) whose connect made it, when one did.
    pub made_by: Option<String>,
    /// The last thing it did that the hub keeps: a page, a token, a step in its chain.
    pub last_active: Option<u64>,
    /// AI apps and terminals connected now (each live refresh-token family).
    pub clients: usize,
}

/// `tenant`'s view, inside a transaction scoped to it.
async fn view_in(
    connection: &mut AnyConnection,
    tenant: &str,
    access: Access,
    now: u64,
) -> Result<TenantView, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, provisional, last_active FROM auth_accounts WHERE tenant_id = $1
         ORDER BY created_at, id",
    )
    .bind(tenant)
    .fetch_all(&mut *connection)
    .await?;
    let mut accounts = Vec::new();
    let mut secured = false;
    let mut last_active: Option<u64> = None;
    for row in &rows {
        accounts.push(row.try_get::<String, _>(0)?);
        secured |= row.try_get::<i64, _>(1)? == 0;
        last_active = last_active.max(row.try_get::<Option<i64>, _>(2)?.map(unsecs));
    }
    let sign_ins = sqlx::query(
        "SELECT provider, email FROM auth_identities WHERE tenant_id = $1
         ORDER BY created_at, provider, subject",
    )
    .bind(tenant)
    .fetch_all(&mut *connection)
    .await?
    .iter()
    .map(|row| {
        Ok(Linked {
            provider: row.try_get(0)?,
            email: row.try_get(1)?,
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()?;
    let made_by: Option<String> = sqlx::query_scalar(
        "SELECT detail FROM tenancy_events WHERE tenant_id = $1
           AND event IN ('client.connected', 'terminal.connected')
         ORDER BY seq LIMIT 1",
    )
    .bind(tenant)
    .fetch_optional(&mut *connection)
    .await?;
    for sql in [
        "SELECT MAX(last_seen) FROM auth_sessions WHERE tenant_id = $1",
        "SELECT MAX(created_at) FROM oauth_tokens WHERE tenant_id = $1",
        // Its own steps: not the hub's or an operator's about it (`tenant.*`).
        "SELECT MAX(at) FROM tenancy_events WHERE tenant_id = $1 AND event NOT LIKE 'tenant.%'",
    ] {
        let latest: Option<i64> = sqlx::query_scalar(sql)
            .bind(tenant)
            .fetch_one(&mut *connection)
            .await?;
        last_active = last_active.max(latest.map(unsecs));
    }
    let clients: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT family) FROM oauth_tokens
         WHERE tenant_id = $1 AND kind = 'refresh' AND used_at IS NULL AND expires_at > $2",
    )
    .bind(tenant)
    .bind(secs(now))
    .fetch_one(&mut *connection)
    .await?;
    Ok(TenantView {
        tenant: tenant.to_string(),
        access,
        accounts,
        secured,
        sign_ins,
        made_by,
        last_active,
        clients: usize::try_from(clients).unwrap_or(0),
    })
}

impl Accounts {
    /// The tenants in `state` (`access::WAITING` or `access::ADMITTED`), at most `most`,
    /// as the operator page shows them: the longest waiting first, the latest let in first.
    /// The home tenant is the operator's own and is left out.
    pub async fn tenant_views(&self, state: &str, most: u32) -> BridgeResult<Vec<TenantView>> {
        let now = now_secs();
        transaction!(&self.store, |connection| {
            let home = tenants::home_in(connection).await?;
            let ids = self
                .store
                .tenants_in(connection, state, i64::from(most) + 1)
                .await?;
            let mut views = Vec::new();
            for tenant in ids.iter().filter(|id| **id != home).take(most as usize) {
                self.store.scope(connection, tenant).await?;
                if let Some((kept, _)) = access::read_in(connection, tenant).await? {
                    views.push(view_in(connection, tenant, kept, now).await?);
                }
            }
            Ok(views)
        })
    }
}

#[cfg(test)]
mod tests;
