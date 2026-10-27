//! The hub's own accounts (D25, §14): people sign in with passkeys on the hub itself, so a
//! hub works with nothing else deployed. Nomen ("Sign in with Shippin") is an extra way in.
//!
//! Increment 32a-1 is the store and the passkey verifier on their own; the sign-in pages
//! that call them come in 32a-2, so until then only the tests do (hence the allow below).
#![allow(dead_code)]

pub mod passkey;

use std::{path::Path, sync::Mutex};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use devagotchi_contracts::{BridgeError, BridgeResult};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::devices::now_secs;

pub use passkey::{Alg, NewCredential};

/// How long a one-time link (the owner's, later invites and resets) stays valid.
pub const LINK_TTL_SECS: u64 = 15 * 60;
/// How long a passkey ceremony may take between its challenge and its answer.
pub const CEREMONY_TTL_SECS: u64 = 5 * 60;
/// Unanswered ceremonies kept at once; more are refused until some expire.
pub const MAX_CEREMONIES: i64 = 10_000;

/// A new account's id: `acct_` and 26 lowercase base32 characters (128 random bits),
/// safe as a directory name wherever an account's files live.
pub fn new_account_id(random: [u8; 16]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::from(ACCOUNT_PREFIX);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in random {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

const ACCOUNT_PREFIX: &str = "acct_";

/// An account id the hub accepts: a new `acct_` id, or a subject an identity provider
/// issued before the hub had accounts of its own (kept as it is, so no history is
/// rewritten). Either way 1 to 64 of `[A-Za-z0-9_-]`, safe as a directory name.
pub fn valid_account_id(id: &str) -> bool {
    if let Some(rest) = id.strip_prefix(ACCOUNT_PREFIX) {
        return rest.len() == 26
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b));
    }
    crate::auth::valid_org(id)
}

/// What a one-time link is for. The owner's link is the only one so far; invites and
/// reset links (decisions 5 and 7) join it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkPurpose {
    Owner,
}

impl LinkPurpose {
    fn as_str(self) -> &'static str {
        match self {
            LinkPurpose::Owner => "owner",
        }
    }
}

/// What a passkey challenge was issued for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ceremony {
    Register,
    SignIn,
}

impl Ceremony {
    fn as_str(self) -> &'static str {
        match self {
            Ceremony::Register => "register",
            Ceremony::SignIn => "sign_in",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub owner: bool,
    pub disabled_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passkey {
    pub id: Vec<u8>,
    pub account: String,
    pub label: String,
    pub public_key: Vec<u8>,
    pub alg: Alg,
    pub sign_count: u32,
    pub backup_eligible: bool,
}

pub struct Accounts {
    connection: Mutex<Connection>,
}

fn store_error(error: rusqlite::Error) -> BridgeError {
    BridgeError::new("account_store", error.to_string())
}

fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.trim().as_bytes()))
}

fn account_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get(0)?,
        name: row.get(1)?,
        owner: row.get::<_, i64>(2)? == 1,
        disabled_at: row.get(3)?,
    })
}

fn passkey_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Passkey> {
    let alg: i64 = row.get(4)?;
    Ok(Passkey {
        id: row.get(0)?,
        account: row.get(1)?,
        label: row.get(2)?,
        public_key: row.get(3)?,
        // Only accepted algorithms are ever stored; an unknown one reads as unusable.
        alg: Alg::from_cose(alg).ok_or(rusqlite::Error::IntegralValueOutOfRange(4, alg))?,
        sign_count: row.get(5)?,
        backup_eligible: row.get::<_, i64>(6)? == 1,
    })
}

const ACCOUNT_COLUMNS: &str = "id, name, owner, disabled_at";
const PASSKEY_COLUMNS: &str = "id, account, label, public_key, alg, sign_count, backup_eligible";

impl Accounts {
    pub fn open(path: &Path) -> BridgeResult<Self> {
        let connection = Connection::open(path).map_err(store_error)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = FULL;
                 PRAGMA busy_timeout = 5000;
                 PRAGMA foreign_keys = ON;
                 CREATE TABLE IF NOT EXISTS accounts (
                   id TEXT PRIMARY KEY,
                   name TEXT NOT NULL,
                   owner INTEGER NOT NULL DEFAULT 0,
                   email TEXT,
                   email_verified_at INTEGER,
                   created_at INTEGER NOT NULL,
                   disabled_at INTEGER
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS one_owner ON accounts (owner) WHERE owner = 1;
                 CREATE TABLE IF NOT EXISTS links (
                   hash TEXT PRIMARY KEY,
                   purpose TEXT NOT NULL,
                   account TEXT NOT NULL REFERENCES accounts (id),
                   expires_at INTEGER NOT NULL,
                   used_at INTEGER
                 );
                 CREATE TABLE IF NOT EXISTS ceremonies (
                   hash TEXT PRIMARY KEY,
                   purpose TEXT NOT NULL,
                   account TEXT,
                   challenge BLOB NOT NULL,
                   expires_at INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS passkeys (
                   id BLOB PRIMARY KEY,
                   account TEXT NOT NULL REFERENCES accounts (id),
                   label TEXT NOT NULL,
                   public_key BLOB NOT NULL,
                   alg INTEGER NOT NULL,
                   sign_count INTEGER NOT NULL,
                   backup_eligible INTEGER NOT NULL,
                   created_at INTEGER NOT NULL,
                   last_used_at INTEGER
                 );",
            )
            .map_err(store_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Make an account. There is at most one owner, and an id is never reused.
    pub fn create(&self, id: &str, name: &str, owner: bool) -> BridgeResult<Account> {
        if !valid_account_id(id) {
            return Err(BridgeError::new(
                "invalid_account",
                format!("{id:?} is not an account id"),
            ));
        }
        let name = name.trim();
        if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
            return Err(BridgeError::new(
                "invalid_account",
                "a name is 1 to 64 printable characters",
            ));
        }
        let mut connection = self.connection.lock().expect("accounts");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let exists: bool = transaction
            .query_row("SELECT 1 FROM accounts WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .optional()
            .map_err(store_error)?
            .is_some();
        if exists {
            return Err(BridgeError::new(
                "account_exists",
                format!("{id} already exists"),
            ));
        }
        if owner {
            let has_owner = transaction
                .query_row("SELECT 1 FROM accounts WHERE owner = 1", [], |_| Ok(()))
                .optional()
                .map_err(store_error)?
                .is_some();
            if has_owner {
                return Err(BridgeError::new(
                    "owner_exists",
                    "this hub already has an owner",
                ));
            }
        }
        transaction
            .execute(
                "INSERT INTO accounts (id, name, owner, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, name, i64::from(owner), now_secs()],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        Ok(Account {
            id: id.to_string(),
            name: name.to_string(),
            owner,
            disabled_at: None,
        })
    }

    pub fn get(&self, id: &str) -> BridgeResult<Option<Account>> {
        self.connection
            .lock()
            .expect("accounts")
            .query_row(
                &format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?1"),
                params![id],
                account_row,
            )
            .optional()
            .map_err(store_error)
    }

    pub fn owner(&self) -> BridgeResult<Option<Account>> {
        self.connection
            .lock()
            .expect("accounts")
            .query_row(
                &format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE owner = 1"),
                [],
                account_row,
            )
            .optional()
            .map_err(store_error)
    }

    /// Issue a one-time link for `account`. Only its SHA-256 is kept; a new link of the
    /// same purpose supersedes the account's unused one.
    pub fn issue_link(
        &self,
        purpose: LinkPurpose,
        account: &str,
        random: [u8; 32],
    ) -> BridgeResult<String> {
        self.issue_link_at(purpose, account, random, now_secs())
    }

    pub(crate) fn issue_link_at(
        &self,
        purpose: LinkPurpose,
        account: &str,
        random: [u8; 32],
        now: u64,
    ) -> BridgeResult<String> {
        let token = format!("dgl_{}", URL_SAFE_NO_PAD.encode(random));
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
        if purpose == LinkPurpose::Owner && !holder.owner {
            return Err(BridgeError::new(
                "not_owner",
                format!("{account} is not the owner"),
            ));
        }
        transaction
            .execute(
                "UPDATE links SET used_at = ?1 WHERE purpose = ?2 AND account = ?3 AND used_at IS NULL",
                params![now, purpose.as_str(), account],
            )
            .map_err(store_error)?;
        transaction
            .execute(
                "INSERT INTO links (hash, purpose, account, expires_at) VALUES (?1, ?2, ?3, ?4)",
                params![hash(&token), purpose.as_str(), account, now + LINK_TTL_SECS],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        Ok(token)
    }

    /// The account a link is for, when it is unused and unexpired. Checking does not
    /// spend it: a link preview cannot burn it.
    pub fn check_link(&self, purpose: LinkPurpose, token: &str) -> BridgeResult<String> {
        self.check_link_at(purpose, token, now_secs())
    }

    pub(crate) fn check_link_at(
        &self,
        purpose: LinkPurpose,
        token: &str,
        now: u64,
    ) -> BridgeResult<String> {
        let connection = self.connection.lock().expect("accounts");
        live_link(&connection, purpose, token, now)
    }

    /// Store a passkey for `account`. With a link, the link must still be live and for this
    /// account, and it is spent in the same transaction: both happen or neither does.
    pub fn add_passkey(
        &self,
        account: &str,
        credential: &NewCredential,
        label: &str,
        link: Option<(LinkPurpose, &str)>,
    ) -> BridgeResult<()> {
        let now = now_secs();
        let label = label.trim();
        if label.is_empty() || label.len() > 64 || label.chars().any(char::is_control) {
            return Err(BridgeError::new(
                "invalid_passkey",
                "a passkey's name is 1 to 64 printable characters",
            ));
        }
        let mut connection = self.connection.lock().expect("accounts");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let exists = transaction
            .query_row(
                "SELECT 1 FROM accounts WHERE id = ?1",
                params![account],
                |_| Ok(()),
            )
            .optional()
            .map_err(store_error)?
            .is_some();
        if !exists {
            return Err(BridgeError::new(
                "no_account",
                format!("no account {account}"),
            ));
        }
        if let Some((purpose, token)) = link {
            if live_link(&transaction, purpose, token, now)? != account {
                return Err(BridgeError::new(
                    "invalid_link",
                    "this link is for another account",
                ));
            }
            transaction
                .execute(
                    "UPDATE links SET used_at = ?1 WHERE hash = ?2",
                    params![now, hash(token)],
                )
                .map_err(store_error)?;
        }
        let taken = transaction
            .query_row(
                "SELECT 1 FROM passkeys WHERE id = ?1",
                params![credential.id],
                |_| Ok(()),
            )
            .optional()
            .map_err(store_error)?
            .is_some();
        if taken {
            return Err(BridgeError::new(
                "passkey_exists",
                "this passkey is already registered",
            ));
        }
        transaction
            .execute(
                "INSERT INTO passkeys (id, account, label, public_key, alg, sign_count, backup_eligible, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    credential.id,
                    account,
                    label,
                    credential.public_key,
                    credential.alg.cose(),
                    credential.sign_count,
                    i64::from(credential.backup_eligible),
                    now
                ],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)
    }

    /// Start a ceremony: remember its challenge under the SHA-256 of a fresh id, which the
    /// browser carries back. Expired ceremonies are dropped on the way.
    pub fn begin_ceremony(
        &self,
        purpose: Ceremony,
        account: Option<&str>,
        id_random: [u8; 16],
        challenge: [u8; 32],
    ) -> BridgeResult<String> {
        self.begin_ceremony_at(purpose, account, id_random, challenge, now_secs())
    }

    pub(crate) fn begin_ceremony_at(
        &self,
        purpose: Ceremony,
        account: Option<&str>,
        id_random: [u8; 16],
        challenge: [u8; 32],
        now: u64,
    ) -> BridgeResult<String> {
        let id = URL_SAFE_NO_PAD.encode(id_random);
        let connection = self.connection.lock().expect("accounts");
        connection
            .execute(
                "DELETE FROM ceremonies WHERE expires_at <= ?1",
                params![now],
            )
            .map_err(store_error)?;
        let open: i64 = connection
            .query_row("SELECT COUNT(*) FROM ceremonies", [], |row| row.get(0))
            .map_err(store_error)?;
        if open >= MAX_CEREMONIES {
            return Err(BridgeError::new(
                "busy",
                "too many sign-ins at once; try again shortly",
            ));
        }
        connection
            .execute(
                "INSERT INTO ceremonies (hash, purpose, account, challenge, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    hash(&id),
                    purpose.as_str(),
                    account,
                    challenge.to_vec(),
                    now + CEREMONY_TTL_SECS
                ],
            )
            .map_err(store_error)?;
        Ok(id)
    }

    /// Finish a ceremony, once: its account (if it was for one) and its challenge.
    pub fn finish_ceremony(
        &self,
        purpose: Ceremony,
        id: &str,
    ) -> BridgeResult<(Option<String>, [u8; 32])> {
        self.finish_ceremony_at(purpose, id, now_secs())
    }

    pub(crate) fn finish_ceremony_at(
        &self,
        purpose: Ceremony,
        id: &str,
        now: u64,
    ) -> BridgeResult<(Option<String>, [u8; 32])> {
        let invalid = || BridgeError::new("invalid_ceremony", "sign-in timed out; start again");
        let mut connection = self.connection.lock().expect("accounts");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let row: Option<(Option<String>, Vec<u8>, u64)> = transaction
            .query_row(
                "SELECT account, challenge, expires_at FROM ceremonies WHERE hash = ?1 AND purpose = ?2",
                params![hash(id), purpose.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(store_error)?;
        let (account, challenge, expires_at) = row.ok_or_else(invalid)?;
        transaction
            .execute("DELETE FROM ceremonies WHERE hash = ?1", params![hash(id)])
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        if now >= expires_at {
            return Err(invalid());
        }
        let challenge: [u8; 32] = challenge.try_into().map_err(|_| invalid())?;
        Ok((account, challenge))
    }

    pub fn passkey(&self, id: &[u8]) -> BridgeResult<Option<Passkey>> {
        self.connection
            .lock()
            .expect("accounts")
            .query_row(
                &format!("SELECT {PASSKEY_COLUMNS} FROM passkeys WHERE id = ?1"),
                params![id],
                passkey_row,
            )
            .optional()
            .map_err(store_error)
    }

    pub fn passkeys_of(&self, account: &str) -> BridgeResult<Vec<Passkey>> {
        let connection = self.connection.lock().expect("accounts");
        let mut statement = connection
            .prepare(&format!(
                "SELECT {PASSKEY_COLUMNS} FROM passkeys WHERE account = ?1 ORDER BY created_at"
            ))
            .map_err(store_error)?;
        let rows = statement
            .query_map(params![account], passkey_row)
            .map_err(store_error)?;
        rows.collect::<Result<_, _>>().map_err(store_error)
    }

    /// Record a sign-in with a passkey: its new signature counter and when.
    pub fn used_passkey(&self, id: &[u8], sign_count: u32) -> BridgeResult<()> {
        self.connection
            .lock()
            .expect("accounts")
            .execute(
                "UPDATE passkeys SET sign_count = ?1, last_used_at = ?2 WHERE id = ?3",
                params![sign_count, now_secs(), id],
            )
            .map_err(store_error)?;
        Ok(())
    }

    /// Remove one of `account`'s passkeys; false when it has no such passkey.
    pub fn remove_passkey(&self, account: &str, id: &[u8]) -> BridgeResult<bool> {
        let removed = self
            .connection
            .lock()
            .expect("accounts")
            .execute(
                "DELETE FROM passkeys WHERE id = ?1 AND account = ?2",
                params![id, account],
            )
            .map_err(store_error)?;
        Ok(removed == 1)
    }
}

fn live_link(
    connection: &Connection,
    purpose: LinkPurpose,
    token: &str,
    now: u64,
) -> BridgeResult<String> {
    let invalid = || BridgeError::new("invalid_link", "this link is used, expired or unknown");
    let row: Option<(String, u64, Option<u64>)> = connection
        .query_row(
            "SELECT account, expires_at, used_at FROM links WHERE hash = ?1 AND purpose = ?2",
            params![hash(token), purpose.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(store_error)?;
    match row {
        Some((account, expires_at, None)) if now < expires_at => Ok(account),
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests;
