//! Browser sessions on the hub's own accounts (32a-2): the browser holds a random id in a
//! `__Host-` cookie; the hub keeps only its SHA-256, the account, and the passkey that
//! opened it. A session ends after [`SESSION_IDLE_SECS`] unused, at [`SESSION_MAX_SECS`]
//! however much it is used, when its passkey is removed, when the account is disabled,
//! when a one-time link for the account is spent, or when its person signs out.

use super::*;

/// A session nobody has used for this long ends.
pub const SESSION_IDLE_SECS: u64 = 30 * 24 * 3600;
/// No session lasts longer than this, however often it is used.
pub const SESSION_MAX_SECS: u64 = 90 * 24 * 3600;
/// Sessions one account keeps at once; past it the least recently used one ends.
pub const MAX_SESSIONS_PER_ACCOUNT: usize = 50;

#[derive(Debug, Clone)]
pub struct Session {
    pub account: Account,
    #[allow(dead_code)] // shown on the account page (32a-3)
    pub created_at: u64,
    pub last_seen: u64,
}

impl Accounts {
    /// Open a session on `account`, opened with `passkey` (none for a session a one-time
    /// link opened). Returns the token for the browser's cookie; only its hash is kept.
    pub async fn open_session(&self, account: &str, passkey: Option<&[u8]>) -> BridgeResult<String> {
        self.open_session_at(account, passkey, crate::link::random::<32>(), now_secs())
            .await
    }

    pub(crate) async fn open_session_at(
        &self,
        account: &str,
        passkey: Option<&[u8]>,
        random: [u8; 32],
        now: u64,
    ) -> BridgeResult<String> {
        transaction!(&self.store, |connection| {
                self.enter(connection, crate::store::Lookup::Account(account)).await?;
                let holder = account_in(connection, account)
                    .await?
                    .ok_or_else(|| BridgeError::new("no_account", format!("no account {account}")))?;
                if holder.disabled_at.is_some() {
                    return Err(account_disabled(account).into());
                }
                insert_session(connection, &holder.tenant, account, passkey, random, now).await
            })
    }

    /// The live session `token` names, if any: not idle too long, not past its limit, on
    /// an account that is not disabled. Seeing it counts as using it.
    pub async fn session(&self, token: &str) -> BridgeResult<Option<Session>> {
        self.session_at(token, now_secs()).await
    }

    pub(crate) async fn session_at(&self, token: &str, now: u64) -> BridgeResult<Option<Session>> {
        if !token.starts_with("dgs_") || token.len() > 128 {
            return Ok(None);
        }
        let key = hash(token);
        transaction!(&self.store, |connection| {
            if self
                .enter(connection, crate::store::Lookup::Session(&key))
                .await?
                .is_none()
            {
                return Ok(None);
            }
            let found = sqlx::query(concat!(
                "SELECT ",
                account_columns_a!(),
                ", s.created_at, s.last_seen, s.expires_at
                 FROM auth_sessions s JOIN auth_accounts a ON a.id = s.account
                 WHERE s.hash = $1"
            ))
            .bind(&key)
            .fetch_optional(&mut *connection)
            .await?
            .map(|row| -> Result<(Session, u64), sqlx::Error> {
                Ok((
                    Session {
                        account: account_row(&row)?,
                        created_at: unsecs(row.try_get(7)?),
                        last_seen: unsecs(row.try_get(8)?),
                    },
                    unsecs(row.try_get(9)?),
                ))
            })
            .transpose()?;
            let Some((mut session, expires_at)) = found else {
                return Ok(None);
            };
            // Past its limit, idle too long, on a disabled account, or on an account nobody
            // secured that went unused too long: it has ended, for good.
            if now >= expires_at
                || now >= session.last_seen.saturating_add(SESSION_IDLE_SECS)
                || session.account.disabled_at.is_some()
                || self.lapsed(&session.account, now)
            {
                sqlx::query("DELETE FROM auth_sessions WHERE hash = $1")
                    .bind(&key)
                    .execute(&mut *connection)
                    .await?;
                return Ok(None);
            }
            // Written at most once a minute per session, not on every page.
            if now >= session.last_seen + 60 {
                sqlx::query("UPDATE auth_sessions SET last_seen = $1 WHERE hash = $2")
                    .bind(secs(now))
                    .bind(&key)
                    .execute(&mut *connection)
                    .await?;
                session.last_seen = now;
                tenants::touch(connection, &mut session.account, now).await?;
            }
            Ok(Some(session))
        })
    }

    /// Add a passkey on a link's authority and open the session it signs in to, in one
    /// transaction: if the session cannot be stored, the link stays live and no passkey is
    /// added. Returns the session's token.
    pub async fn register_with_link(
        &self,
        account: &str,
        credential: &NewCredential,
        label: &str,
        purpose: LinkPurpose,
        token: &str,
    ) -> BridgeResult<String> {
        self.put_passkey(
            account,
            credential,
            label,
            Some((purpose, token)),
            None,
            now_secs(),
            Some(crate::link::random::<32>()),
        )
        .await?
        .ok_or_else(|| BridgeError::new("account_store", "no session was opened"))
    }

    /// End one session (signing out); false when there was none.
    pub async fn end_session(&self, token: &str) -> BridgeResult<bool> {
        let key = hash(token);
        transaction!(&self.store, |connection| {
            self.enter(connection, crate::store::Lookup::Session(&key)).await?;
            let removed = sqlx::query("DELETE FROM auth_sessions WHERE hash = $1")
                .bind(&key)
                .execute(&mut *connection)
                .await?
                .rows_affected();
            Ok(removed == 1)
        })
    }

    /// End every session of `account`; how many there were.
    #[allow(dead_code)] // "sign out everywhere" on the account page (32a-3)
    pub async fn end_sessions(&self, account: &str) -> BridgeResult<u64> {
        transaction!(&self.store, |connection| {
            self.enter(connection, crate::store::Lookup::Account(account)).await?;
            Ok(sqlx::query("DELETE FROM auth_sessions WHERE account = $1")
                .bind(account)
                .execute(&mut *connection)
                .await?
                .rows_affected())
        })
    }
}

/// Store a new session of `account` (of `tenant`, which the caller's transaction is scoped
/// to): expired sessions are cleared, and past the cap the account's least recently used
/// ones make room. Returns its token.
pub(super) async fn insert_session(
    connection: &mut AnyConnection,
    tenant: &str,
    account: &str,
    passkey: Option<&[u8]>,
    random: [u8; 32],
    now: u64,
) -> Result<String, Failed> {
    let token = format!("dgs_{}", URL_SAFE_NO_PAD.encode(random));
    sqlx::query("DELETE FROM auth_sessions WHERE expires_at <= $1 OR last_seen + $2 <= $1")
        .bind(secs(now))
        .bind(secs(SESSION_IDLE_SECS))
        .execute(&mut *connection)
        .await?;
    sqlx::query(
        "DELETE FROM auth_sessions WHERE account = $1 AND hash NOT IN (
           SELECT hash FROM auth_sessions WHERE account = $1
           ORDER BY last_seen DESC, created_at DESC LIMIT $2)",
    )
    .bind(account)
    .bind(secs(MAX_SESSIONS_PER_ACCOUNT as u64 - 1))
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO auth_sessions (hash, account, passkey, created_at, last_seen, expires_at, tenant_id)
         VALUES ($1, $2, $3, $4, $4, $5, $6)",
    )
    .bind(hash(&token))
    .bind(account)
    .bind(passkey)
    .bind(secs(now))
    .bind(secs(now + SESSION_MAX_SECS))
    .bind(tenant)
    .execute(&mut *connection)
    .await?;
    Ok(token)
}

#[cfg(test)]
mod tests;
