//! Devagotchi accounts. The hub's own users, not an identity provider's `sub`.
//!
//! A sign-in proves `(issuer, sub)` and, when the provider says so, an email.
//! Resolution is: that pair already linked, else a verified email already on
//! file, else a new account and its personal workspace. An unverified email
//! never selects an account. Races lose to the unique keys and try again.

use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use devagotchi_contracts::{BridgeError, BridgeResult};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::config::valid_subject;

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA busy_timeout = 5000;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS accounts (
  id TEXT PRIMARY KEY,
  display_name TEXT,
  local_email TEXT,
  created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS accounts_local_email
  ON accounts(local_email) WHERE local_email IS NOT NULL;
CREATE TABLE IF NOT EXISTS workspaces (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS memberships (
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  account_id TEXT NOT NULL REFERENCES accounts(id),
  role TEXT NOT NULL,
  PRIMARY KEY (workspace_id, account_id)
);
CREATE TABLE IF NOT EXISTS linked_identities (
  issuer TEXT NOT NULL,
  sub TEXT NOT NULL,
  account_id TEXT NOT NULL REFERENCES accounts(id),
  email TEXT,
  email_verified INTEGER NOT NULL,
  linked_at INTEGER NOT NULL,
  PRIMARY KEY (issuer, sub)
);
CREATE INDEX IF NOT EXISTS linked_identities_account
  ON linked_identities(account_id);
CREATE TABLE IF NOT EXISTS verified_emails (
  email TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id)
);
CREATE TABLE IF NOT EXISTS identity_events (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL,
  action TEXT NOT NULL,
  account_id TEXT NOT NULL,
  issuer TEXT,
  sub TEXT,
  detail TEXT NOT NULL
);
";

/// How a resolved sign-in relates to the account that was already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// `(issuer, sub)` was already on this account.
    Existing,
    /// A verified email matched an account; this subject is now linked to it.
    Linked,
    /// Nothing matched. A new account and its personal workspace were made.
    Created,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub account_id: String,
    pub workspace_id: String,
    pub outcome: Outcome,
}

/// What an upstream provider, or a local sign-in, proved.
#[derive(Debug, Clone)]
pub struct Claim {
    pub issuer: String,
    pub sub: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub issuer: String,
    pub sub: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub linked_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub action: String,
    pub account_id: String,
    pub issuer: Option<String>,
    pub sub: Option<String>,
    pub detail: String,
}

struct Norm {
    issuer: String,
    sub: String,
    email: Option<String>,
    email_verified: bool,
    name: Option<String>,
}

pub struct Store {
    connection: Mutex<Connection>,
}

fn store_error(error: rusqlite::Error) -> BridgeError {
    if let rusqlite::Error::SqliteFailure(code, _) = &error
        && code.code == rusqlite::ErrorCode::ConstraintViolation
    {
        return BridgeError::new("constraint", "identity row already exists");
    }
    BridgeError::new("identity_store", error.to_string())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn is_account_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("dg_") else {
        return false;
    };
    rest.len() == 32
        && rest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}{}", hex::encode(crate::link::random::<16>()))
}

fn normalize_issuer(issuer: &str) -> BridgeResult<String> {
    let issuer = issuer.trim().trim_end_matches('/').to_string();
    if issuer.is_empty() || issuer.len() > 256 {
        return Err(BridgeError::new(
            "invalid_identity",
            "issuer must be 1..=256 characters",
        ));
    }
    if issuer.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(BridgeError::new(
            "invalid_identity",
            "issuer has whitespace or control characters",
        ));
    }
    if !issuer.contains("://") && !issuer.starts_with("urn:") {
        return Err(BridgeError::new(
            "invalid_identity",
            "issuer must be an absolute URL or a urn",
        ));
    }
    Ok(issuer)
}

fn normalize_email(email: &str) -> BridgeResult<String> {
    let email = email.trim().to_lowercase();
    if email.len() > 254 || email.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(BridgeError::new("invalid_identity", "email is not usable"));
    }
    let Some((local, domain)) = email.split_once('@') else {
        return Err(BridgeError::new("invalid_identity", "email is not usable"));
    };
    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return Err(BridgeError::new("invalid_identity", "email is not usable"));
    }
    Ok(email)
}

fn normalize_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return None;
    }
    Some(name.to_string())
}

fn normalize(claim: &Claim) -> BridgeResult<Norm> {
    let issuer = normalize_issuer(&claim.issuer)?;
    valid_subject(&claim.sub).map_err(|why| BridgeError::new("invalid_identity", why))?;
    let email = match &claim.email {
        Some(email) => Some(normalize_email(email)?),
        None => None,
    };
    Ok(Norm {
        issuer,
        sub: claim.sub.clone(),
        email,
        email_verified: claim.email_verified,
        name: claim.name.as_deref().and_then(normalize_name),
    })
}

impl Store {
    pub fn open(path: &Path) -> BridgeResult<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                BridgeError::new("identity_store", format!("{}: {error}", parent.display()))
            })?;
        }
        if !path.exists() {
            crate::push::write_private(path, &[])?;
        }
        let connection = Connection::open(path).map_err(store_error)?;
        connection.execute_batch(SCHEMA).map_err(store_error)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
                params![now_secs() as i64],
            )
            .map_err(store_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// The account for this sign-in. See the module comment for the order.
    pub fn resolve(&self, claim: &Claim) -> BridgeResult<Resolved> {
        let claim = normalize(claim)?;
        for _ in 0..4 {
            match self.resolve_once(&claim) {
                Err(error) if error.code == "constraint" => continue,
                other => return other,
            }
        }
        Err(BridgeError::new(
            "constraint",
            "identity resolve lost a race",
        ))
    }

    /// An account id named by config. Created, with a personal workspace, when absent.
    pub fn ensure_account(&self, id: &str) -> BridgeResult<String> {
        if !is_account_id(id) {
            return Err(BridgeError::new(
                "invalid_identity",
                "account id must be dg_ and 32 lowercase hex characters",
            ));
        }
        for _ in 0..4 {
            match self.ensure_account_once(id) {
                Err(error) if error.code == "constraint" => continue,
                other => return other,
            }
        }
        Err(BridgeError::new(
            "constraint",
            "could not ensure the account",
        ))
    }

    /// Prove `email` out of band (a redeemed magic link) and sign in as its account.
    pub fn redeem_local(&self, email: &str, name: Option<&str>) -> BridgeResult<Resolved> {
        let email = normalize_email(email)?;
        let name = name.and_then(normalize_name);
        for _ in 0..4 {
            match self.redeem_local_once(&email, name.as_deref()) {
                Err(error) if error.code == "constraint" => continue,
                other => return other,
            }
        }
        Err(BridgeError::new("constraint", "local sign-in lost a race"))
    }

    /// Drop one linked provider. Fails when the account would have no way to sign in.
    pub fn unlink(&self, account_id: &str, issuer: &str, sub: &str) -> BridgeResult<()> {
        let issuer = normalize_issuer(issuer)?;
        valid_subject(sub).map_err(|why| BridgeError::new("invalid_identity", why))?;
        let mut connection = self.connection.lock().expect("identity lock");
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let linked: Option<(String, Option<String>, bool)> = tx
            .query_row(
                "SELECT account_id, email, email_verified FROM linked_identities
                 WHERE issuer = ?1 AND sub = ?2",
                params![issuer, sub],
                |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? != 0)),
            )
            .optional()
            .map_err(store_error)?;
        let Some((owner, email, verified)) = linked else {
            return Err(BridgeError::new(
                "not_linked",
                "that sign-in is not linked to an account",
            ));
        };
        if owner != account_id {
            return Err(BridgeError::new(
                "not_linked",
                "that sign-in belongs to another account",
            ));
        }
        let others: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM linked_identities WHERE account_id = ?1 AND NOT (issuer = ?2 AND sub = ?3)",
                params![account_id, issuer, sub],
                |row| row.get(0),
            )
            .map_err(store_error)?;
        let local: Option<String> = tx
            .query_row(
                "SELECT local_email FROM accounts WHERE id = ?1",
                params![account_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_error)?
            .flatten();
        if others == 0 && local.is_none() {
            return Err(BridgeError::new(
                "last_way_in",
                "refusing to unlink the only way into this account",
            ));
        }
        tx.execute(
            "DELETE FROM linked_identities WHERE issuer = ?1 AND sub = ?2",
            params![issuer, sub],
        )
        .map_err(store_error)?;
        if verified {
            if let Some(email) = email.as_deref() {
                let still = email_still_held(&tx, account_id, email)?;
                if !still {
                    tx.execute(
                        "DELETE FROM verified_emails WHERE email = ?1 AND account_id = ?2",
                        params![email, account_id],
                    )
                    .map_err(store_error)?;
                }
            }
        }
        record(
            &tx,
            "unlink",
            account_id,
            Some(&issuer),
            Some(sub),
            "unlinked",
        )?;
        tx.commit().map_err(store_error)?;
        Ok(())
    }

    pub fn identities(&self, account_id: &str) -> BridgeResult<Vec<Identity>> {
        let connection = self.connection.lock().expect("identity lock");
        let mut statement = connection
            .prepare(
                "SELECT issuer, sub, email, email_verified, linked_at FROM linked_identities
                 WHERE account_id = ?1 ORDER BY linked_at, issuer, sub",
            )
            .map_err(store_error)?;
        let rows = statement
            .query_map(params![account_id], |row| {
                Ok(Identity {
                    issuer: row.get(0)?,
                    sub: row.get(1)?,
                    email: row.get(2)?,
                    email_verified: row.get::<_, i64>(3)? != 0,
                    linked_at: row.get::<_, i64>(4)? as u64,
                })
            })
            .map_err(store_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
    }

    pub fn events(&self, account_id: &str) -> BridgeResult<Vec<Event>> {
        let connection = self.connection.lock().expect("identity lock");
        let mut statement = connection
            .prepare(
                "SELECT action, account_id, issuer, sub, detail FROM identity_events
                 WHERE account_id = ?1 ORDER BY seq",
            )
            .map_err(store_error)?;
        let rows = statement
            .query_map(params![account_id], |row| {
                Ok(Event {
                    action: row.get(0)?,
                    account_id: row.get(1)?,
                    issuer: row.get(2)?,
                    sub: row.get(3)?,
                    detail: row.get(4)?,
                })
            })
            .map_err(store_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
    }

    pub fn local_email(&self, account_id: &str) -> BridgeResult<Option<String>> {
        let connection = self.connection.lock().expect("identity lock");
        connection
            .query_row(
                "SELECT local_email FROM accounts WHERE id = ?1",
                params![account_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_error)
            .map(|row| row.flatten())
    }

    fn resolve_once(&self, claim: &Norm) -> BridgeResult<Resolved> {
        let mut connection = self.connection.lock().expect("identity lock");
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let resolved = resolve_tx(&tx, claim)?;
        tx.commit().map_err(store_error)?;
        Ok(resolved)
    }

    fn ensure_account_once(&self, id: &str) -> BridgeResult<String> {
        let mut connection = self.connection.lock().expect("identity lock");
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        if account_exists(&tx, id)? {
            return Ok(id.to_string());
        }
        insert_account(&tx, id, None, None)?;
        tx.commit().map_err(store_error)?;
        Ok(id.to_string())
    }

    fn redeem_local_once(&self, email: &str, name: Option<&str>) -> BridgeResult<Resolved> {
        let mut connection = self.connection.lock().expect("identity lock");
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        if let Some(account_id) = email_owner(&tx, email)? {
            let previous: Option<String> = tx
                .query_row(
                    "SELECT local_email FROM accounts WHERE id = ?1",
                    params![account_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(store_error)?
                .flatten();
            if previous.as_deref() != Some(email) {
                tx.execute(
                    "UPDATE accounts SET local_email = ?1 WHERE id = ?2 AND local_email IS NULL",
                    params![email, account_id],
                )
                .map_err(store_error)?;
            }
            if let Some(name) = name {
                tx.execute(
                    "UPDATE accounts SET display_name = ?1 WHERE id = ?2 AND display_name IS NULL",
                    params![name, account_id],
                )
                .map_err(store_error)?;
            }
            record(
                &tx,
                "link",
                &account_id,
                None,
                None,
                &format!("local email {email}"),
            )?;
            let workspace_id = personal_workspace(&tx, &account_id)?;
            tx.commit().map_err(store_error)?;
            return Ok(Resolved {
                account_id,
                workspace_id,
                outcome: Outcome::Linked,
            });
        }
        let account_id = new_id("dg_");
        insert_account(&tx, &account_id, name, Some(email))?;
        // The mailbox is verified by the act of redeeming the link.
        tx.execute(
            "INSERT INTO verified_emails (email, account_id) VALUES (?1, ?2)",
            params![email, account_id],
        )
        .map_err(store_error)?;
        record(
            &tx,
            "create",
            &account_id,
            None,
            None,
            &format!("local email {email}"),
        )?;
        let workspace_id = personal_workspace(&tx, &account_id)?;
        tx.commit().map_err(store_error)?;
        Ok(Resolved {
            account_id,
            workspace_id,
            outcome: Outcome::Created,
        })
    }
}

fn resolve_tx(tx: &Transaction<'_>, claim: &Norm) -> BridgeResult<Resolved> {
    if let Some(account_id) = linked_account(tx, &claim.issuer, &claim.sub)? {
        note_email_on_existing(tx, claim, &account_id)?;
        let workspace_id = personal_workspace(tx, &account_id)?;
        return Ok(Resolved {
            account_id,
            workspace_id,
            outcome: Outcome::Existing,
        });
    }
    if claim.email_verified
        && let Some(email) = &claim.email
        && let Some(account_id) = email_owner(tx, email)?
    {
        insert_identity(tx, claim, &account_id)?;
        record(
            tx,
            "link",
            &account_id,
            Some(&claim.issuer),
            Some(&claim.sub),
            &format!("verified email {email}"),
        )?;
        let workspace_id = personal_workspace(tx, &account_id)?;
        return Ok(Resolved {
            account_id,
            workspace_id,
            outcome: Outcome::Linked,
        });
    }
    let account_id = new_id("dg_");
    insert_account(tx, &account_id, claim.name.as_deref(), None)?;
    insert_identity(tx, claim, &account_id)?;
    if claim.email_verified
        && let Some(email) = &claim.email
    {
        tx.execute(
            "INSERT INTO verified_emails (email, account_id) VALUES (?1, ?2)",
            params![email, account_id],
        )
        .map_err(store_error)?;
    }
    record(
        tx,
        "create",
        &account_id,
        Some(&claim.issuer),
        Some(&claim.sub),
        "created",
    )?;
    let workspace_id = personal_workspace(tx, &account_id)?;
    Ok(Resolved {
        account_id,
        workspace_id,
        outcome: Outcome::Created,
    })
}

/// An identity that is already linked does not move. A verified email that
/// belongs to another account is recorded and left where it is. A verified
/// email nobody owns yet is attached to this account.
fn note_email_on_existing(
    tx: &Transaction<'_>,
    claim: &Norm,
    account_id: &str,
) -> BridgeResult<()> {
    let Some(email) = claim.email.as_ref().filter(|_| claim.email_verified) else {
        return Ok(());
    };
    match email_owner(tx, email)? {
        Some(owner) if owner != account_id => {
            record(
                tx,
                "email_conflict",
                account_id,
                Some(&claim.issuer),
                Some(&claim.sub),
                &format!("verified email {email} belongs to another account"),
            )?;
        }
        Some(_) => {
            tx.execute(
                "UPDATE linked_identities SET email = ?1, email_verified = 1
                 WHERE issuer = ?2 AND sub = ?3 AND email IS NULL",
                params![email, claim.issuer, claim.sub],
            )
            .map_err(store_error)?;
        }
        None => {
            tx.execute(
                "INSERT INTO verified_emails (email, account_id) VALUES (?1, ?2)",
                params![email, account_id],
            )
            .map_err(store_error)?;
            tx.execute(
                "UPDATE linked_identities SET email = ?1, email_verified = 1
                 WHERE issuer = ?2 AND sub = ?3",
                params![email, claim.issuer, claim.sub],
            )
            .map_err(store_error)?;
        }
    }
    Ok(())
}

fn insert_account(
    tx: &Transaction<'_>,
    id: &str,
    name: Option<&str>,
    local_email: Option<&str>,
) -> BridgeResult<()> {
    let now = now_secs() as i64;
    tx.execute(
        "INSERT INTO accounts (id, display_name, local_email, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, name, local_email, now],
    )
    .map_err(store_error)?;
    let workspace_id = new_id("ws_");
    tx.execute(
        "INSERT INTO workspaces (id, name, kind, created_at) VALUES (?1, 'Personal', 'personal', ?2)",
        params![workspace_id, now],
    )
    .map_err(store_error)?;
    tx.execute(
        "INSERT INTO memberships (workspace_id, account_id, role) VALUES (?1, ?2, 'owner')",
        params![workspace_id, id],
    )
    .map_err(store_error)?;
    record(tx, "create", id, None, None, "personal workspace")?;
    Ok(())
}

fn insert_identity(tx: &Transaction<'_>, claim: &Norm, account_id: &str) -> BridgeResult<()> {
    let (email, verified) = if claim.email_verified {
        (claim.email.as_deref(), 1)
    } else {
        (None, 0)
    };
    tx.execute(
        "INSERT INTO linked_identities
         (issuer, sub, account_id, email, email_verified, linked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            claim.issuer,
            claim.sub,
            account_id,
            email,
            verified,
            now_secs() as i64
        ],
    )
    .map_err(store_error)?;
    Ok(())
}

fn linked_account(tx: &Transaction<'_>, issuer: &str, sub: &str) -> BridgeResult<Option<String>> {
    tx.query_row(
        "SELECT account_id FROM linked_identities WHERE issuer = ?1 AND sub = ?2",
        params![issuer, sub],
        |row| row.get(0),
    )
    .optional()
    .map_err(store_error)
}

fn email_owner(tx: &Transaction<'_>, email: &str) -> BridgeResult<Option<String>> {
    tx.query_row(
        "SELECT account_id FROM verified_emails WHERE email = ?1",
        params![email],
        |row| row.get(0),
    )
    .optional()
    .map_err(store_error)
}

fn account_exists(tx: &Transaction<'_>, id: &str) -> BridgeResult<bool> {
    tx.query_row("SELECT 1 FROM accounts WHERE id = ?1", params![id], |_| {
        Ok(())
    })
    .optional()
    .map_err(store_error)
    .map(|row| row.is_some())
}

fn personal_workspace(tx: &Transaction<'_>, account_id: &str) -> BridgeResult<String> {
    tx.query_row(
        "SELECT w.id FROM workspaces w
         JOIN memberships m ON m.workspace_id = w.id
         WHERE m.account_id = ?1 AND w.kind = 'personal'",
        params![account_id],
        |row| row.get(0),
    )
    .map_err(store_error)
}

fn email_still_held(tx: &Transaction<'_>, account_id: &str, email: &str) -> BridgeResult<bool> {
    let on_identity: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM linked_identities
             WHERE account_id = ?1 AND email = ?2 AND email_verified = 1",
            params![account_id, email],
            |row| row.get(0),
        )
        .map_err(store_error)?;
    if on_identity > 0 {
        return Ok(true);
    }
    let local: Option<String> = tx
        .query_row(
            "SELECT local_email FROM accounts WHERE id = ?1",
            params![account_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_error)?
        .flatten();
    Ok(local.as_deref() == Some(email))
}

fn record(
    tx: &Transaction<'_>,
    action: &str,
    account_id: &str,
    issuer: Option<&str>,
    sub: Option<&str>,
    detail: &str,
) -> BridgeResult<()> {
    tx.execute(
        "INSERT INTO identity_events (at, action, account_id, issuer, sub, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now_secs() as i64, action, account_id, issuer, sub, detail],
    )
    .map_err(store_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    fn claim(issuer: &str, sub: &str, email: Option<&str>, verified: bool) -> Claim {
        Claim {
            issuer: issuer.into(),
            sub: sub.into(),
            email: email.map(str::to_string),
            email_verified: verified,
            name: Some("Ada".into()),
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("identities.sqlite3")).unwrap();
        (dir, store)
    }

    #[test]
    fn a_new_subject_gets_an_account_and_a_personal_workspace() {
        let (_dir, store) = store();
        let first = store
            .resolve(&claim("https://idp.example", "user-1", None, false))
            .unwrap();
        assert_eq!(first.outcome, Outcome::Created);
        assert!(is_account_id(&first.account_id));
        assert!(first.workspace_id.starts_with("ws_"));
        let again = store
            .resolve(&claim("https://idp.example/", "user-1", None, false))
            .unwrap();
        assert_eq!(again.outcome, Outcome::Existing);
        assert_eq!(again.account_id, first.account_id);
        assert_eq!(again.workspace_id, first.workspace_id);
        let events = store.events(&first.account_id).unwrap();
        assert!(events.iter().any(|event| event.action == "create"));
    }

    #[test]
    fn a_verified_email_links_a_second_issuer_and_an_unverified_one_does_not() {
        let (_dir, store) = store();
        let first = store
            .resolve(&claim(
                "https://nomen.example",
                "sub-a",
                Some("Ada@Example.com"),
                true,
            ))
            .unwrap();
        let second = store
            .resolve(&claim(
                "https://google.example",
                "sub-b",
                Some("ada@example.com"),
                true,
            ))
            .unwrap();
        assert_eq!(second.outcome, Outcome::Linked);
        assert_eq!(second.account_id, first.account_id);
        let identities = store.identities(&first.account_id).unwrap();
        assert_eq!(identities.len(), 2);
        assert!(identities.iter().all(|identity| identity.email_verified));

        let stranger = store
            .resolve(&claim(
                "https://other.example",
                "sub-c",
                Some("ada@example.com"),
                false,
            ))
            .unwrap();
        assert_eq!(stranger.outcome, Outcome::Created);
        assert_ne!(stranger.account_id, first.account_id);
        let stranger_ids = store.identities(&stranger.account_id).unwrap();
        assert_eq!(stranger_ids.len(), 1);
        assert_eq!(stranger_ids[0].email, None);
        assert!(!stranger_ids[0].email_verified);
    }

    #[test]
    fn an_existing_link_is_not_moved_when_its_email_belongs_to_someone_else() {
        let (_dir, store) = store();
        let owner = store
            .resolve(&claim(
                "https://idp.example",
                "owner",
                Some("shared@example.com"),
                true,
            ))
            .unwrap();
        let other = store
            .resolve(&claim("https://idp.example", "other", None, false))
            .unwrap();
        let kept = store
            .resolve(&claim(
                "https://idp.example",
                "other",
                Some("shared@example.com"),
                true,
            ))
            .unwrap();
        assert_eq!(kept.outcome, Outcome::Existing);
        assert_eq!(kept.account_id, other.account_id);
        let events = store.events(&other.account_id).unwrap();
        assert!(events.iter().any(|event| event.action == "email_conflict"));
        let still = store
            .resolve(&claim(
                "https://google.example",
                "new",
                Some("shared@example.com"),
                true,
            ))
            .unwrap();
        assert_eq!(still.account_id, owner.account_id);
    }

    #[test]
    fn unlink_refuses_the_last_way_in_and_allows_one_of_two() {
        let (_dir, store) = store();
        let account = store
            .resolve(&claim(
                "https://idp.example",
                "user-1",
                Some("ada@example.com"),
                true,
            ))
            .unwrap();
        let refused = store.unlink(&account.account_id, "https://idp.example", "user-1");
        assert_eq!(refused.unwrap_err().code, "last_way_in");
        store
            .resolve(&claim(
                "https://google.example",
                "user-2",
                Some("ada@example.com"),
                true,
            ))
            .unwrap();
        store
            .unlink(&account.account_id, "https://idp.example", "user-1")
            .unwrap();
        assert_eq!(store.identities(&account.account_id).unwrap().len(), 1);
        let back = store
            .resolve(&claim(
                "https://idp.example",
                "user-1",
                Some("ada@example.com"),
                true,
            ))
            .unwrap();
        assert_eq!(back.outcome, Outcome::Linked);
        assert_eq!(back.account_id, account.account_id);
    }

    #[test]
    fn a_local_email_is_a_way_in_and_links_to_the_verified_account() {
        let (_dir, store) = store();
        let created = store.redeem_local("Ada@Example.com", Some("Ada")).unwrap();
        assert_eq!(created.outcome, Outcome::Created);
        let again = store.redeem_local("ada@example.com", None).unwrap();
        assert_eq!(again.account_id, created.account_id);
        assert_eq!(again.outcome, Outcome::Linked);
        let upstream = store
            .resolve(&claim(
                "https://idp.example",
                "user-1",
                Some("ada@example.com"),
                true,
            ))
            .unwrap();
        assert_eq!(upstream.account_id, created.account_id);
        store
            .unlink(&created.account_id, "https://idp.example", "user-1")
            .unwrap();
        assert!(store.identities(&created.account_id).unwrap().is_empty());
        assert_eq!(
            store.local_email(&created.account_id).unwrap().as_deref(),
            Some("ada@example.com")
        );
    }

    #[test]
    fn concurrent_first_sign_ins_on_one_email_are_one_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identities.sqlite3");
        Store::open(&path).unwrap();
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();
        for i in 0..8 {
            let path = path.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let store = Store::open(&path).unwrap();
                barrier.wait();
                store
                    .resolve(&claim(
                        &format!("https://idp{i}.example"),
                        &format!("user-{i}"),
                        Some("ada@example.com"),
                        true,
                    ))
                    .unwrap()
            }));
        }
        let mut ids = std::collections::BTreeSet::new();
        for handle in handles {
            ids.insert(handle.join().unwrap().account_id);
        }
        assert_eq!(ids.len(), 1, "{ids:?}");
    }

    #[test]
    fn a_named_account_id_is_created_once() {
        let (_dir, store) = store();
        let id = format!("dg_{}", "ab".repeat(16));
        assert_eq!(store.ensure_account(&id).unwrap(), id);
        assert_eq!(store.ensure_account(&id).unwrap(), id);
        assert!(store.ensure_account("not-an-id").is_err());
        assert!(store.ensure_account("dg_ABC").is_err());
    }

    #[test]
    fn garbage_claims_are_refused() {
        let (_dir, store) = store();
        assert!(
            store
                .resolve(&claim("idp.example", "user-1", None, false))
                .is_err()
        );
        assert!(
            store
                .resolve(&claim("https://idp.example", "has space", None, false))
                .is_err()
        );
        assert!(
            store
                .resolve(&claim(
                    "https://idp.example",
                    "user-1",
                    Some("not-an-email"),
                    true
                ))
                .is_err()
        );
    }
}
