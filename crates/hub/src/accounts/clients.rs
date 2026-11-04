//! The AI clients a tenant has working (migration 0011): which families of AI client
//! (`analytics::funnel::client_family`: claude, chatgpt, grok, cursor) made a successful MCP
//! call with one of the tenant's tokens. What a workflow's `ai_client` step waits on
//! (`crate::workflows`). Only the family and when are kept.

use super::*;

/// How often a family's latest call is written again; the first one always is.
pub const NOTE_EVERY_SECS: u64 = 3600;

impl Accounts {
    /// `family` made a successful MCP call for `tenant` at `now`: kept once, its latest
    /// refreshed. Returns whether this is the tenant's first call from that family.
    pub async fn note_client_call(&self, tenant: &str, family: &str, now: u64) -> BridgeResult<bool> {
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            let made = sqlx::query(
                "INSERT INTO tenancy_clients (tenant_id, family, first_call_at, last_call_at)
                 VALUES ($1, $2, $3, $3) ON CONFLICT (tenant_id, family) DO NOTHING",
            )
            .bind(tenant)
            .bind(family)
            .bind(secs(now))
            .execute(&mut *connection)
            .await?
            .rows_affected();
            if made == 0 {
                sqlx::query(
                    "UPDATE tenancy_clients SET last_call_at = $1
                     WHERE tenant_id = $2 AND family = $3 AND last_call_at < $1",
                )
                .bind(secs(now))
                .bind(tenant)
                .bind(family)
                .execute(&mut *connection)
                .await?;
            }
            Ok(made > 0)
        })
    }

    /// The families of AI client that made a successful MCP call for `tenant`.
    pub async fn client_families(&self, tenant: &str) -> BridgeResult<Vec<String>> {
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            Ok(sqlx::query_scalar(
                "SELECT family FROM tenancy_clients WHERE tenant_id = $1 ORDER BY family",
            )
            .bind(tenant)
            .fetch_all(&mut *connection)
            .await?)
        })
    }
}
