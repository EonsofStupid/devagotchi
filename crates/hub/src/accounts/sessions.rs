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
                let holder = account_in(connection, account)
                    .await?
                    .ok_or_else(|| BridgeError::new("no_account", format!("no account {account}")))?;
                if holder.disabled_at.is_some() {
                    return Err(account_disabled(account).into());
                }
                insert_session(connection, account, passkey, random, now).await
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
        let mut connection = self.store.read().await?;
        let found = sqlx::query(
            "SELECT a.id, a.name, a.owner, a.disabled_at, s.created_at, s.last_seen, s.expires_at
             FROM sessions s JOIN accounts a ON a.id = s.account
             WHERE s.hash = $1",
        )
        .bind(&key)
        .fetch_optional(&mut *connection)
        .await
        .and_then(|row| {
            row.map(|row| -> Result<(Session, u64), sqlx::Error> {
                Ok((
                    Session {
                        account: account_row(&row)?,
                        created_at: unsecs(row.try_get(4)?),
                        last_seen: unsecs(row.try_get(5)?),
                    },
                    unsecs(row.try_get(6)?),
                ))
            })
            .transpose()
        })
        .map_err(store_error)?;
        let Some((mut session, expires_at)) = found else {
            return Ok(None);
        };
        // Past its limit, idle too long, or on a disabled account: it has ended, for good.
        if now >= expires_at
            || now >= session.last_seen.saturating_add(SESSION_IDLE_SECS)
            || session.account.disabled_at.is_some()
        {
            sqlx::query("DELETE FROM sessions WHERE hash = $1")
                .bind(&key)
                .execute(&mut *connection)
                .await
                .map_err(store_error)?;
            return Ok(None);
        }
        // Written at most once a minute per session, not on every page.
        if now >= session.last_seen + 60 {
            sqlx::query("UPDATE sessions SET last_seen = $1 WHERE hash = $2")
                .bind(secs(now))
                .bind(&key)
                .execute(&mut *connection)
                .await
                .map_err(store_error)?;
            session.last_seen = now;
        }
        Ok(Some(session))
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
            now_secs(),
            Some(crate::link::random::<32>()),
        )
        .await?
        .ok_or_else(|| BridgeError::new("account_store", "no session was opened"))
    }

    /// End one session (signing out); false when there was none.
    pub async fn end_session(&self, token: &str) -> BridgeResult<bool> {
        let mut connection = self.store.read().await?;
        let removed = sqlx::query("DELETE FROM sessions WHERE hash = $1")
            .bind(hash(token))
            .execute(&mut *connection)
            .await
            .map_err(store_error)?
            .rows_affected();
        Ok(removed == 1)
    }

    /// End every session of `account`; how many there were.
    #[allow(dead_code)] // "sign out everywhere" on the account page (32a-3)
    pub async fn end_sessions(&self, account: &str) -> BridgeResult<u64> {
        let mut connection = self.store.read().await?;
        sqlx::query("DELETE FROM sessions WHERE account = $1")
            .bind(account)
            .execute(&mut *connection)
            .await
            .map(|done| done.rows_affected())
            .map_err(store_error)
    }
}

/// Store a new session of `account` inside the caller's transaction: expired sessions are
/// cleared, and past the cap the account's least recently used ones make room. Returns its
/// token.
pub(super) async fn insert_session(
    connection: &mut AnyConnection,
    account: &str,
    passkey: Option<&[u8]>,
    random: [u8; 32],
    now: u64,
) -> Result<String, Failed> {
    let token = format!("dgs_{}", URL_SAFE_NO_PAD.encode(random));
    sqlx::query("DELETE FROM sessions WHERE expires_at <= $1 OR last_seen + $2 <= $1")
        .bind(secs(now))
        .bind(secs(SESSION_IDLE_SECS))
        .execute(&mut *connection)
        .await?;
    sqlx::query(
        "DELETE FROM sessions WHERE account = $1 AND hash NOT IN (
           SELECT hash FROM sessions WHERE account = $1
           ORDER BY last_seen DESC, created_at DESC LIMIT $2)",
    )
    .bind(account)
    .bind(secs(MAX_SESSIONS_PER_ACCOUNT as u64 - 1))
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO sessions (hash, account, passkey, created_at, last_seen, expires_at)
         VALUES ($1, $2, $3, $4, $4, $5)",
    )
    .bind(hash(&token))
    .bind(account)
    .bind(passkey)
    .bind(secs(now))
    .bind(secs(now + SESSION_MAX_SECS))
    .execute(&mut *connection)
    .await?;
    Ok(token)
}

#[cfg(test)]
mod tests;
