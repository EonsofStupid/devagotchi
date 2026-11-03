//! Tenants: each person's own space on a hub (the design adopted 2 Nov, "each user
//! self-contained"). Every account belongs to exactly one tenant `t-<uuid>`, as Shippin's
//! tenants are named; a tenant has its own data, limits and audit chain. Tenants are pooled
//! in one store by default (`isolation` says `pooled`; a silo is the same rows in a store of
//! their own). Optional tenant hostnames are `<slug>.<tenancy.domain>`.
//!
//! - The hub's first tenant is `home`: its operator's (Jesse is tenant #1 on the hosted
//!   hub), and on a one-owner hub (`tenancy.mode single`) the only one. Migration 0007 put
//!   the accounts made before tenants in it; `settle_in` makes it on a new store.
//! - On Postgres the database keeps tenants apart (`store.rs`, row-level security): every
//!   query here runs in a transaction scoped to one tenant and sees no other tenant's rows.
//! - On a hosted hub (`tenancy.mode multi`) the first connect from an AI client or the CLI
//!   makes the person's account and its own tenant on the spot (`start_unsecured`): the
//!   anonymous-then-link account upgrade (an anonymous account first, a real sign-in linked
//!   to it later, as Firebase's anonymous accounts and Auth0's account linking do). Until
//!   then it is `provisional`, held to `tenancy.unsecured` (per-tenant quotas) and ends
//!   once unused for `idle_days`.
//! - Each tenant's events are a hash chain of their own (`tenancy_events`): one tenant's
//!   history is never mixed into another's, and an edit to any row breaks the chain after it.

use std::net::IpAddr;

use crate::analytics::funnel;

use super::*;

const TENANT_PREFIX: &str = "t-";

/// A new tenant id: `t-` and a random (version 4) UUID.
pub fn new_tenant_id(random: [u8; 16]) -> String {
    let mut bytes = random;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    format!(
        "{TENANT_PREFIX}{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A tenant id the hub made: `t-` and a lowercase UUID.
pub fn valid_tenant_id(id: &str) -> bool {
    let Some(uuid) = id.strip_prefix(TENANT_PREFIX) else {
        return false;
    };
    uuid.len() == 36
        && uuid.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

/// A short, stable tag of a tenant for names that must not collide across tenants (machine
/// names on a hosted hub): `t` and the first 12 hex digits of its UUID.
pub fn tag(tenant: &str) -> String {
    let hex: String = tenant
        .strip_prefix(TENANT_PREFIX)
        .unwrap_or(tenant)
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(12)
        .collect();
    format!("t{hex}")
}

/// Make the home tenant (`home`, a new id) if there is none. Inside the caller's
/// transaction; idempotent. Migration 0007 put the accounts made before tenants in it
/// (making it when there was none); its chain says so the first time the hub opens it.
pub(super) async fn settle_in(
    store: &Store,
    connection: &mut AnyConnection,
    home: &str,
    now: u64,
) -> Result<(), Failed> {
    sqlx::query(
        "INSERT INTO tenancy_tenants (id, home, created_at) SELECT $1, 1, $2
         WHERE NOT EXISTS (SELECT 1 FROM tenancy_tenants WHERE home = 1)",
    )
    .bind(home)
    .bind(secs(now))
    .execute(&mut *connection)
    .await?;
    let home = home_in(connection).await?;
    store.scope(connection, &home).await?;
    // The operator's tenant is always in (`access.rs`).
    super::access::home_in(connection, &home, now).await?;
    let told = sqlx::query("SELECT 1 FROM tenancy_events WHERE tenant_id = $1 LIMIT 1")
        .bind(&home)
        .fetch_optional(&mut *connection)
        .await?
        .is_some();
    if !told {
        let joined: i64 = sqlx::query("SELECT COUNT(*) FROM auth_accounts WHERE tenant_id = $1")
            .bind(&home)
            .fetch_one(&mut *connection)
            .await?
            .try_get(0)?;
        if joined > 0 {
            record_in(
                connection,
                &home,
                "hub",
                "tenant.accounts_joined",
                &format!("{joined} accounts from before tenants"),
                now,
            )
            .await?;
        }
    }
    Ok(())
}

/// The home tenant's id.
pub(super) async fn home_in(connection: &mut AnyConnection) -> Result<String, Failed> {
    Ok(sqlx::query("SELECT id FROM tenancy_tenants WHERE home = 1")
        .fetch_one(&mut *connection)
        .await?
        .try_get(0)?)
}

/// One step in a tenant's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub seq: u64,
    pub at: u64,
    pub actor: String,
    pub event: String,
    pub detail: String,
}

fn event_hash(
    prev: &str,
    tenant: &str,
    seq: i64,
    at: i64,
    actor: &str,
    event: &str,
    detail: &str,
) -> String {
    let mut digest = Sha256::new();
    for part in [
        prev,
        tenant,
        &seq.to_string(),
        &at.to_string(),
        actor,
        event,
        detail,
    ] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    hex::encode(digest.finalize())
}

/// Add an event to `tenant`'s chain, inside the caller's transaction (so it is recorded
/// exactly when what it records is), which is scoped to `tenant`.
pub(super) async fn record_in(
    connection: &mut AnyConnection,
    tenant: &str,
    actor: &str,
    event: &str,
    detail: &str,
    now: u64,
) -> Result<(), Failed> {
    let head = sqlx::query(
        "SELECT seq, hash FROM tenancy_events WHERE tenant_id = $1 ORDER BY seq DESC LIMIT 1",
    )
    .bind(tenant)
    .fetch_optional(&mut *connection)
    .await?;
    let (seq, prev) = match head {
        Some(row) => (row.try_get::<i64, _>(0)? + 1, row.try_get::<String, _>(1)?),
        None => (1, "0".repeat(64)),
    };
    let at = secs(now);
    let hash = event_hash(&prev, tenant, seq, at, actor, event, detail);
    sqlx::query(
        "INSERT INTO tenancy_events (tenant_id, seq, at, actor, event, detail, prev_hash, hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(tenant)
    .bind(seq)
    .bind(at)
    .bind(actor)
    .bind(event)
    .bind(detail)
    .bind(&prev)
    .bind(&hash)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// Note that `account` was used, for an account nobody secured (its idle clock), at most
/// once a minute.
pub(super) async fn touch(
    connection: &mut AnyConnection,
    account: &mut Account,
    now: u64,
) -> Result<(), sqlx::Error> {
    if !account.provisional || account.last_active.is_some_and(|at| now < at + 60) {
        return Ok(());
    }
    sqlx::query("UPDATE auth_accounts SET last_active = $1 WHERE id = $2")
        .bind(secs(now))
        .bind(&account.id)
        .execute(&mut *connection)
        .await?;
    account.last_active = Some(now);
    Ok(())
}

/// The account is secured now (`how`: a passkey, or a linked sign-in): it is no longer
/// provisional, and its tenant's chain says so (and, `counting`, its secured milestone).
/// Nothing for an account that already was.
pub(super) async fn secure_in(
    connection: &mut AnyConnection,
    account: &Account,
    how: &str,
    counting: bool,
    now: u64,
) -> Result<(), Failed> {
    let secured =
        sqlx::query("UPDATE auth_accounts SET provisional = 0 WHERE id = $1 AND provisional = 1")
            .bind(&account.id)
            .execute(&mut *connection)
            .await?
            .rows_affected();
    if secured == 1 {
        record_in(
            connection,
            &account.tenant,
            &account.id,
            "account.secured",
            how,
            now,
        )
        .await?;
        if counting {
            funnel::note_in(connection, &account.tenant, funnel::SECURED, how, now).await?;
        }
    }
    Ok(())
}

impl Accounts {
    /// Whether `account` was made on first connect, never secured, and has gone unused for
    /// `tenancy.unsecured.idle_days`: then it has ended, and its sessions and tokens with it.
    pub fn lapsed(&self, account: &Account, now: u64) -> bool {
        account.provisional
            && now
                >= account
                    .last_active
                    .unwrap_or(0)
                    .saturating_add(self.tenancy.unsecured.idle_secs())
    }

    /// When an account not secured yet ends if it stays unused; `None` once it is secured.
    pub fn lapses_at(&self, account: &Account) -> Option<u64> {
        account.provisional.then(|| {
            account
                .last_active
                .unwrap_or(0)
                .saturating_add(self.tenancy.unsecured.idle_secs())
        })
    }

    /// Who `account` is on a request, signed in `via` a passkey session or a hub token:
    /// its tenant, and while nobody has secured it, when it lapses and whether that is
    /// near (under a seventh of the idle time, an hour at least).
    pub fn subject(&self, account: &Account, via: &'static str) -> crate::auth::Subject {
        let lapses = self.lapses_at(account);
        let idle = self.tenancy.unsecured.idle_secs();
        crate::auth::Subject {
            id: account.id.clone(),
            via,
            tenant: Some(account.tenant.clone()).filter(|tenant| !tenant.is_empty()),
            name: Some(account.name.clone()),
            org: None,
            provisional_until: lapses,
            agent: None,
            claim_code: None,
            provisional_warning: lapses
                .is_some_and(|until| until.saturating_sub(now_secs()) < (idle / 7).max(3600)),
            access: account.access.clone(),
        }
    }

    /// The hub's first tenant: its operator's, and a one-owner hub's only one.
    pub async fn home_tenant(&self) -> BridgeResult<String> {
        transaction!(&self.store, |connection| { home_in(connection).await })
    }

    /// Make a person's account and its own tenant, the first time they connect an AI client
    /// or the CLI to a hosted hub, and open the browser session it is bound to: one click,
    /// no form. `client` is what they were connecting (for the tenant's chain). Refused past
    /// `tenancy.starts_per_hour` from one client address (`rate_limited`) or past
    /// `tenancy.max_unsecured` live accounts nobody secured (`capacity`).
    pub async fn start_unsecured(
        &self,
        address: IpAddr,
        client: &str,
    ) -> BridgeResult<(Account, String)> {
        self.start_unsecured_at(
            address,
            client,
            crate::link::random::<16>(),
            crate::link::random::<16>(),
            crate::link::random::<32>(),
            now_secs(),
        )
        .await
    }

    pub(crate) async fn start_unsecured_at(
        &self,
        address: IpAddr,
        client: &str,
        tenant_random: [u8; 16],
        account_random: [u8; 16],
        session_random: [u8; 32],
        now: u64,
    ) -> BridgeResult<(Account, String)> {
        if !self.tenancy.multi() {
            return Err(BridgeError::new(
                "not_hosted",
                "this hub makes accounts only for its owner's invitations",
            ));
        }
        let source = self.address_key(address);
        let tenant = new_tenant_id(tenant_random);
        let id = new_account_id(account_random);
        let client: String = client
            .chars()
            .filter(|c| !c.is_control())
            .take(128)
            .collect();
        let per_hour = i64::from(self.tenancy.starts_per_hour);
        let cap = i64::from(self.tenancy.max_unsecured);
        let idle = secs(self.tenancy.unsecured.idle_secs());
        let token = transaction!(&self.store, |connection| {
            sqlx::query("DELETE FROM auth_account_starts WHERE at <= $1")
                .bind(secs(now) - 3600)
                .execute(&mut *connection)
                .await?;
            let recent: i64 =
                sqlx::query("SELECT COUNT(*) FROM auth_account_starts WHERE source = $1")
                    .bind(&source)
                    .fetch_one(&mut *connection)
                    .await?
                    .try_get(0)?;
            if recent >= per_hour {
                return Err(BridgeError::new(
                    "rate_limited",
                    "too many new accounts from this address; try again in an hour",
                )
                .into());
            }
            // Across every tenant: the one count the hub's role may make (`store.rs`).
            let live = self
                .store
                .live_unsecured(connection, secs(now) - idle)
                .await?;
            if live >= cap {
                return Err(BridgeError::new(
                    "capacity",
                    "this hub is not making new accounts right now; try again later",
                )
                .into());
            }
            sqlx::query("INSERT INTO auth_account_starts (source, at) VALUES ($1, $2)")
                .bind(&source)
                .bind(secs(now))
                .execute(&mut *connection)
                .await?;
            sqlx::query("INSERT INTO tenancy_tenants (id, created_at) VALUES ($1, $2)")
                .bind(&tenant)
                .bind(secs(now))
                .execute(&mut *connection)
                .await?;
            self.store.scope(connection, &tenant).await?;
            record_in(
                connection,
                &tenant,
                "hub",
                "tenant.created",
                "first connect",
                now,
            )
            .await?;
            let access =
                super::access::begin_in(connection, &tenant, &self.site(), self.counting, now)
                    .await?;
            sqlx::query(
                "INSERT INTO auth_accounts (id, name, owner, created_at, tenant_id, provisional, last_active)
                 VALUES ($1, $2, 0, $3, $4, 1, $3)",
            )
            .bind(&id)
            .bind(UNSECURED_NAME)
            .bind(secs(now))
            .bind(&tenant)
            .execute(&mut *connection)
            .await?;
            record_in(
                connection,
                &tenant,
                &id,
                "account.created",
                &format!("on first connect, from {client}; not secured"),
                now,
            )
            .await?;
            if self.counting {
                let family = funnel::client_family(&client);
                funnel::note_in(connection, &tenant, funnel::MADE, family, now).await?;
            }
            let token =
                sessions::insert_session(connection, &tenant, &id, None, session_random, now)
                    .await?;
            record_in(
                connection,
                &tenant,
                &id,
                "session.opened",
                "this browser",
                now,
            )
            .await?;
            Ok((token, access))
        })?;
        let (token, access) = token;
        Ok((
            Account {
                id,
                name: UNSECURED_NAME.to_string(),
                tenant,
                provisional: true,
                last_active: Some(now),
                access: Some(access),
                ..Account::default()
            },
            token,
        ))
    }

    /// The rate-limit key of a client address: an HMAC under the store's own key, so the
    /// table never holds an address.
    fn address_key(&self, address: IpAddr) -> String {
        let address = match address {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
            v4 => v4,
        };
        let tag = hmac::sign(
            &self.challenge_key.read().expect("challenge key"),
            format!("devagotchi account start\0{address}").as_bytes(),
        );
        hex::encode(&tag.as_ref()[..16])
    }

    /// Record a step in `tenant`'s chain on its own.
    pub async fn record(
        &self,
        tenant: &str,
        actor: &str,
        event: &str,
        detail: &str,
    ) -> BridgeResult<()> {
        let now = now_secs();
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            record_in(connection, tenant, actor, event, detail, now).await
        })
    }

    /// The latest `limit` events of `tenant`, newest first.
    pub async fn events(&self, tenant: &str, limit: u32) -> BridgeResult<Vec<Event>> {
        transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            Ok(sqlx::query(
                "SELECT seq, at, actor, event, detail FROM tenancy_events
                 WHERE tenant_id = $1 ORDER BY seq DESC LIMIT $2",
            )
            .bind(tenant)
            .bind(i64::from(limit))
            .fetch_all(&mut *connection)
            .await
            .and_then(|rows| {
                rows.iter()
                    .map(|row| {
                        Ok(Event {
                            seq: unsecs(row.try_get(0)?),
                            at: unsecs(row.try_get(1)?),
                            actor: row.try_get(2)?,
                            event: row.try_get(3)?,
                            detail: row.try_get(4)?,
                        })
                    })
                    .collect::<Result<Vec<_>, sqlx::Error>>()
            })?)
        })
    }

    /// Whether `tenant`'s chain is whole: every row's hash is its own, over the row before.
    pub async fn chain_intact(&self, tenant: &str) -> BridgeResult<bool> {
        let rows = transaction!(&self.store, |connection| {
            self.store.scope(connection, tenant).await?;
            Ok(sqlx::query(
                "SELECT seq, at, actor, event, detail, prev_hash, hash FROM tenancy_events
                 WHERE tenant_id = $1 ORDER BY seq",
            )
            .bind(tenant)
            .fetch_all(&mut *connection)
            .await?)
        })?;
        let mut prev = "0".repeat(64);
        let mut expected = 1i64;
        for row in rows {
            let read = |row: &AnyRow| -> Result<_, sqlx::Error> {
                Ok((
                    row.try_get::<i64, _>(0)?,
                    row.try_get::<i64, _>(1)?,
                    row.try_get::<String, _>(2)?,
                    row.try_get::<String, _>(3)?,
                    row.try_get::<String, _>(4)?,
                    row.try_get::<String, _>(5)?,
                    row.try_get::<String, _>(6)?,
                ))
            };
            let (seq, at, actor, event, detail, prev_hash, hash) =
                read(&row).map_err(store_error)?;
            if seq != expected
                || prev_hash != prev
                || hash != event_hash(&prev, tenant, seq, at, &actor, &event, &detail)
            {
                return Ok(false);
            }
            prev = hash;
            expected += 1;
        }
        Ok(true)
    }
}

/// The name an account made on first connect has until its person gives one.
pub const UNSECURED_NAME: &str = "New account";

#[cfg(test)]
mod tests;
