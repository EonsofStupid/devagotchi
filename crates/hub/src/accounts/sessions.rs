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
    pub fn open_session(&self, account: &str, passkey: Option<&[u8]>) -> BridgeResult<String> {
        self.open_session_at(account, passkey, crate::link::random::<32>(), now_secs())
    }

    pub(crate) fn open_session_at(
        &self,
        account: &str,
        passkey: Option<&[u8]>,
        random: [u8; 32],
        now: u64,
    ) -> BridgeResult<String> {
        let mut connection = self.connection.lock().expect("accounts");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let holder = transaction
            .query_row(
                &format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?1"),
                params![account],
                account_row,
            )
            .optional()
            .map_err(store_error)?
            .ok_or_else(|| BridgeError::new("no_account", format!("no account {account}")))?;
        if holder.disabled_at.is_some() {
            return Err(account_disabled(account));
        }
        let token = insert_session(&transaction, account, passkey, random, now)?;
        transaction.commit().map_err(store_error)?;
        Ok(token)
    }

    /// The live session `token` names, if any: not idle too long, not past its limit, on
    /// an account that is not disabled. Seeing it counts as using it.
    pub fn session(&self, token: &str) -> BridgeResult<Option<Session>> {
        self.session_at(token, now_secs())
    }

    pub(crate) fn session_at(&self, token: &str, now: u64) -> BridgeResult<Option<Session>> {
        if !token.starts_with("dgs_") || token.len() > 128 {
            return Ok(None);
        }
        let key = hash(token);
        let connection = self.connection.lock().expect("accounts");
        let found = connection
            .query_row(
                "SELECT a.id, a.name, a.owner, a.disabled_at, s.created_at, s.last_seen, s.expires_at
                 FROM sessions s JOIN accounts a ON a.id = s.account
                 WHERE s.hash = ?1",
                params![key],
                |row| {
                    Ok((
                        Session {
                            account: account_row(row)?,
                            created_at: row.get(4)?,
                            last_seen: row.get(5)?,
                        },
                        row.get::<_, u64>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(store_error)?;
        let Some((mut session, expires_at)) = found else {
            return Ok(None);
        };
        // Past its limit, idle too long, or on a disabled account: it has ended, for good.
        if now >= expires_at
            || now >= session.last_seen.saturating_add(SESSION_IDLE_SECS)
            || session.account.disabled_at.is_some()
        {
            connection
                .execute("DELETE FROM sessions WHERE hash = ?1", params![key])
                .map_err(store_error)?;
            return Ok(None);
        }
        // Written at most once a minute per session, not on every page.
        if now >= session.last_seen + 60 {
            connection
                .execute(
                    "UPDATE sessions SET last_seen = ?1 WHERE hash = ?2",
                    params![now, key],
                )
                .map_err(store_error)?;
            session.last_seen = now;
        }
        Ok(Some(session))
    }

    /// Add a passkey on a link's authority and open the session it signs in to, in one
    /// transaction: if the session cannot be stored, the link stays live and no passkey is
    /// added. Returns the session's token.
    pub fn register_with_link(
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
        )?
        .ok_or_else(|| BridgeError::new("account_store", "no session was opened"))
    }

    /// End one session (signing out); false when there was none.
    pub fn end_session(&self, token: &str) -> BridgeResult<bool> {
        let removed = self
            .connection
            .lock()
            .expect("accounts")
            .execute("DELETE FROM sessions WHERE hash = ?1", params![hash(token)])
            .map_err(store_error)?;
        Ok(removed == 1)
    }

    /// End every session of `account`; how many there were.
    #[allow(dead_code)] // "sign out everywhere" on the account page (32a-3)
    pub fn end_sessions(&self, account: &str) -> BridgeResult<usize> {
        self.connection
            .lock()
            .expect("accounts")
            .execute("DELETE FROM sessions WHERE account = ?1", params![account])
            .map_err(store_error)
    }
}

/// Store a new session of `account` inside `transaction`: expired sessions are cleared,
/// and past the cap the account's least recently used ones make room. Returns its token.
pub(super) fn insert_session(
    transaction: &Connection,
    account: &str,
    passkey: Option<&[u8]>,
    random: [u8; 32],
    now: u64,
) -> BridgeResult<String> {
    let token = format!("dgs_{}", URL_SAFE_NO_PAD.encode(random));
    transaction
        .execute(
            "DELETE FROM sessions WHERE expires_at <= ?1 OR last_seen + ?2 <= ?1",
            params![now, SESSION_IDLE_SECS],
        )
        .map_err(store_error)?;
    transaction
        .execute(
            "DELETE FROM sessions WHERE hash IN (
               SELECT hash FROM sessions WHERE account = ?1
               ORDER BY last_seen DESC, created_at DESC LIMIT -1 OFFSET ?2)",
            params![account, MAX_SESSIONS_PER_ACCOUNT - 1],
        )
        .map_err(store_error)?;
    transaction
        .execute(
            "INSERT INTO sessions (hash, account, passkey, created_at, last_seen, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![hash(&token), account, passkey, now, now + SESSION_MAX_SECS],
        )
        .map_err(store_error)?;
    Ok(token)
}

#[cfg(test)]
mod tests;
