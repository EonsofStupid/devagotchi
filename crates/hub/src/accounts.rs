//! The hub's own accounts (D25, §14): people sign in with passkeys on the hub itself, so a
//! hub works with nothing else deployed. Nomen ("Sign in with Shippin") is an extra way in.
//!
//! Passkey challenges are the hub's own and cost nothing to hand out: each is a time, a
//! random nonce and an HMAC over them, the ceremony's purpose and (to register) the
//! account, under a key kept in the store. Nothing is written until a ceremony succeeds;
//! then its challenge is recorded as spent, in the same transaction as what it did, until
//! it would have expired anyway. A flood of unanswered sign-ins fills nothing.
//!
//! Increment 32a-1 is the store and the passkey verifier on their own; the sign-in pages
//! that call them come in 32a-2, so until then only the tests do (hence the allow below).
#![allow(dead_code)]

pub mod passkey;

use std::{path::Path, sync::Mutex};

use aws_lc_rs::hmac;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use devagotchi_contracts::{BridgeError, BridgeResult};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::devices::now_secs;

pub use passkey::{Alg, Asserted, NewCredential};

/// How long a one-time link (the owner's, later invites and resets) stays valid.
pub const LINK_TTL_SECS: u64 = 15 * 60;
/// How long a passkey ceremony may take between its challenge and its answer.
pub const CHALLENGE_TTL_SECS: u64 = 5 * 60;
/// A challenge: format (1), issued at (8, big endian), nonce (16), HMAC-SHA-256 (32).
const CHALLENGE_LEN: usize = 57;
const CHALLENGE_FORMAT: u8 = 1;

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

/// What lets a passkey be added to an account: a live one-time link for it. (A signed-in
/// session of the account joins this with the account page.)
#[derive(Clone, Copy)]
pub enum Authority<'a> {
    Link(LinkPurpose, &'a str),
}

/// Never prints the link itself: it is a live secret until spent.
impl std::fmt::Debug for Authority<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Authority::Link(purpose, _) => write!(f, "Link({purpose:?}, <redacted>)"),
        }
    }
}

fn invalid_ceremony() -> BridgeError {
    BridgeError::new("invalid_ceremony", "sign-in timed out; start again")
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
    /// Signs the passkey challenges this process makes: the store's key as last read. The
    /// store's copy is the one that counts: every ceremony checks against it, read in its
    /// own transaction, and refreshes this one. It changes only when a clock is found set
    /// back (`clock_set_back`).
    challenge_key: std::sync::RwLock<hmac::Key>,
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
    /// Open the store at `path`. It holds the challenge key, so it and its -wal and -shm
    /// files are the hub's alone (0600), whether made here or found.
    pub fn open(path: &Path) -> BridgeResult<Self> {
        if !path.exists() {
            // SQLite gives its -wal and -shm files the main file's mode, so all three are 0600.
            crate::push::write_private(path, &[])?;
        }
        make_private(path)?;
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
                 CREATE TABLE IF NOT EXISTS secrets (
                   name TEXT PRIMARY KEY,
                   value BLOB NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS spent_challenges (
                   mac BLOB PRIMARY KEY,
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
        connection
            .execute(
                "INSERT OR IGNORE INTO secrets (name, value) VALUES ('challenge_key', ?1)",
                params![crate::link::random::<32>().to_vec()],
            )
            .map_err(store_error)?;
        let key = stored_key(&connection)?;
        make_private(path)?;
        Ok(Self {
            connection: Mutex::new(connection),
            challenge_key: std::sync::RwLock::new(key),
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

    /// Issue a one-time link for `account`: 256 random bits the store draws itself. Only
    /// its SHA-256 is kept; a new link of the same purpose supersedes the account's unused
    /// one.
    pub fn issue_link(&self, purpose: LinkPurpose, account: &str) -> BridgeResult<String> {
        self.issue_link_at(purpose, account, crate::link::random::<32>(), now_secs())
    }

    fn issue_link_at(
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

    /// Store a passkey for `account`, as `verify_registration` returned it, on the
    /// `authority` of a live link for this account. The challenge it answered must be one
    /// this hub made to register a passkey on this account. The challenge and the link are
    /// spent in the same transaction as the passkey is stored: all of it happens or none
    /// does.
    pub fn add_passkey(
        &self,
        account: &str,
        credential: &NewCredential,
        label: &str,
        authority: Authority<'_>,
    ) -> BridgeResult<()> {
        let Authority::Link(purpose, token) = authority;
        self.store_passkey(
            account,
            credential,
            label,
            Some((purpose, token)),
            now_secs(),
        )
    }

    /// `add_passkey`, with the authority optional: for this module and its tests only.
    fn store_passkey(
        &self,
        account: &str,
        credential: &NewCredential,
        label: &str,
        link: Option<(LinkPurpose, &str)>,
        now: u64,
    ) -> BridgeResult<()> {
        let label = label.trim();
        if label.is_empty() || label.len() > 64 || label.chars().any(char::is_control) {
            return Err(BridgeError::new(
                "invalid_passkey",
                "a passkey's name is 1 to 64 printable characters",
            ));
        }
        let mut connection = self.connection.lock().expect("accounts");
        if self.clock_set_back(&mut connection, now)? {
            return Err(invalid_ceremony());
        }
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let (mac, issued) = self.check_challenge(
            &transaction,
            Ceremony::Register,
            Some(account),
            &credential.challenge,
            now,
        )?;
        spend_challenge(&transaction, &mac, issued, now)?;
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

    /// A challenge for one ceremony: to register a passkey on `account`, or to sign in
    /// (`None`). Making one stores nothing.
    pub fn challenge(&self, purpose: Ceremony, account: Option<&str>) -> Vec<u8> {
        self.challenge_at(purpose, account, crate::link::random::<16>(), now_secs())
    }

    pub(crate) fn challenge_at(
        &self,
        purpose: Ceremony,
        account: Option<&str>,
        nonce: [u8; 16],
        now: u64,
    ) -> Vec<u8> {
        let mut challenge = Vec::with_capacity(CHALLENGE_LEN);
        challenge.push(CHALLENGE_FORMAT);
        challenge.extend_from_slice(&now.to_be_bytes());
        challenge.extend_from_slice(&nonce);
        let tag = hmac::sign(
            &self.challenge_key.read().expect("challenge key"),
            &challenge_message(&challenge, purpose, account),
        );
        challenge.extend_from_slice(tag.as_ref());
        challenge
    }

    /// Check, inside the ceremony's transaction, that `challenge` is this hub's, for
    /// `purpose` (and `account`), and unexpired; returns its MAC, the key it is spent under,
    /// and when it was issued. The key is the one the store holds now, read in the same
    /// transaction, so a key another process replaced is never trusted here; this process
    /// takes it up for the challenges it makes next. Whether the challenge is spent is
    /// checked when it is.
    fn check_challenge(
        &self,
        transaction: &Connection,
        purpose: Ceremony,
        account: Option<&str>,
        challenge: &[u8],
        now: u64,
    ) -> BridgeResult<(Vec<u8>, u64)> {
        let key = stored_key(transaction)?;
        *self.challenge_key.write().expect("challenge key") = key.clone();
        if challenge.len() != CHALLENGE_LEN || challenge[0] != CHALLENGE_FORMAT {
            return Err(invalid_ceremony());
        }
        let (signed, tag) = challenge.split_at(CHALLENGE_LEN - 32);
        hmac::verify(&key, &challenge_message(signed, purpose, account), tag)
            .map_err(|_| invalid_ceremony())?;
        let issued = u64::from_be_bytes(signed[1..9].try_into().expect("8 bytes"));
        if issued > now || now >= issued.saturating_add(CHALLENGE_TTL_SECS) {
            return Err(invalid_ceremony());
        }
        Ok((tag.to_vec(), issued))
    }

    /// Whether the clock is now more than five minutes behind the horizon of cleared spent
    /// challenges: set back after running ahead. Then a challenge made before the step could
    /// be one whose record was cleared, and refusing all of them until the clock catches up
    /// could take hours. Instead the challenge key is replaced (with the connection held, so
    /// no ceremony is between its check and its spending) and the records and horizon start
    /// over: every challenge made before fails once, every one made after works.
    fn clock_set_back(&self, connection: &mut Connection, now: u64) -> BridgeResult<bool> {
        if now.saturating_add(CHALLENGE_TTL_SECS) > spent_horizon(connection)? {
            return Ok(false);
        }
        let key = crate::link::random::<32>();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        transaction
            .execute(
                "UPDATE secrets SET value = ?1 WHERE name = 'challenge_key'",
                params![key.to_vec()],
            )
            .map_err(store_error)?;
        transaction
            .execute_batch(
                "DELETE FROM secrets WHERE name = 'spent_horizon';
                 DELETE FROM spent_challenges;",
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        *self.challenge_key.write().expect("challenge key") =
            hmac::Key::new(hmac::HMAC_SHA256, &key);
        eprintln!(
            "devagotchi-hub: the clock was set back (now {now}); passkey challenges made before were withdrawn"
        );
        Ok(true)
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

    /// Record a sign-in, as `check_sign_in` returned it. In one transaction: the challenge
    /// is a live sign-in challenge of this hub's and is spent; the passkey exists with the
    /// key that verified the signature; its account exists and is not disabled; the user
    /// handle is that account's; and the counter moves forward from what is stored now, so
    /// two sign-ins racing with one copied key cannot both count. Returns the account.
    pub fn sign_in(&self, asserted: &Asserted) -> BridgeResult<Account> {
        self.sign_in_at(asserted, now_secs())
    }

    fn sign_in_at(&self, asserted: &Asserted, now: u64) -> BridgeResult<Account> {
        let mut connection = self.connection.lock().expect("accounts");
        if self.clock_set_back(&mut connection, now)? {
            return Err(invalid_ceremony());
        }
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let (mac, issued) = self.check_challenge(
            &transaction,
            Ceremony::SignIn,
            None,
            &asserted.challenge,
            now,
        )?;
        spend_challenge(&transaction, &mac, issued, now)?;
        let not_here =
            || BridgeError::new("invalid_passkey", "this passkey is not registered here");
        let passkey = transaction
            .query_row(
                &format!("SELECT {PASSKEY_COLUMNS} FROM passkeys WHERE id = ?1"),
                params![asserted.id],
                passkey_row,
            )
            .optional()
            .map_err(store_error)?
            .ok_or_else(not_here)?;
        // The key, algorithm and backup eligibility the signature was checked with must be
        // the ones stored for this passkey.
        if passkey.public_key != asserted.public_key
            || passkey.alg != asserted.alg
            || passkey.backup_eligible != asserted.backup_eligible
        {
            return Err(not_here());
        }
        let account = transaction
            .query_row(
                &format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?1"),
                params![passkey.account],
                account_row,
            )
            .map_err(store_error)?;
        if account.disabled_at.is_some() {
            return Err(account_disabled(&account.id));
        }
        if asserted.user_handle != account.id.as_bytes() {
            return Err(BridgeError::new(
                "user_handle",
                "the passkey answered for another account",
            ));
        }
        // Authenticators that keep no counter (most synced passkeys) report 0 every time.
        let count = asserted.sign_count;
        if (count != 0 || passkey.sign_count != 0) && count <= passkey.sign_count {
            return Err(BridgeError::new(
                "signature_counter",
                "this passkey's counter went backwards: it may have been copied",
            ));
        }
        transaction
            .execute(
                "UPDATE passkeys SET sign_count = ?1, last_used_at = ?2 WHERE id = ?3",
                params![count, now, asserted.id],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        Ok(account)
    }

    /// Remove one of `account`'s passkeys; false when it has no such passkey. Its last one
    /// stays: removing it would lock the account out.
    pub fn remove_passkey(&self, account: &str, id: &[u8]) -> BridgeResult<bool> {
        let mut connection = self.connection.lock().expect("accounts");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let held: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM passkeys WHERE account = ?1",
                params![account],
                |row| row.get(0),
            )
            .map_err(store_error)?;
        let theirs = transaction
            .query_row(
                "SELECT 1 FROM passkeys WHERE id = ?1 AND account = ?2",
                params![id, account],
                |_| Ok(()),
            )
            .optional()
            .map_err(store_error)?
            .is_some();
        if !theirs {
            return Ok(false);
        }
        if held <= 1 {
            return Err(BridgeError::new(
                "last_passkey",
                "this is the account's only passkey; add another before removing it",
            ));
        }
        transaction
            .execute(
                "DELETE FROM passkeys WHERE id = ?1 AND account = ?2",
                params![id, account],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        Ok(true)
    }
}

fn account_disabled(account: &str) -> BridgeError {
    BridgeError::new("account_disabled", format!("{account} is disabled"))
}

/// What a challenge's HMAC covers: its own bytes, the purpose, and the account.
fn challenge_message(signed: &[u8], purpose: Ceremony, account: Option<&str>) -> Vec<u8> {
    let account = account.unwrap_or("");
    let mut message = Vec::with_capacity(64 + signed.len() + account.len());
    message.extend_from_slice(b"devagotchi passkey challenge\0");
    message.extend_from_slice(signed);
    message.extend_from_slice(purpose.as_str().as_bytes());
    message.push(0);
    message.extend_from_slice(account.as_bytes());
    message
}

/// Spend a challenge once, inside the ceremony's transaction. A record is kept until the
/// challenge expires (`issued` + five minutes). Clearing out expired records moves a
/// horizon forward to the latest expiry cleared, and a challenge expiring at or before it is
/// refused from then on: if the clock jumps ahead and back, a challenge whose record was
/// cleared while it looked expired cannot be used again.
fn spend_challenge(connection: &Connection, mac: &[u8], issued: u64, now: u64) -> BridgeResult<()> {
    let expires = issued.saturating_add(CHALLENGE_TTL_SECS);
    let horizon = spent_horizon(connection)?;
    if expires <= horizon {
        return Err(invalid_ceremony());
    }
    let cleared: Option<u64> = connection
        .query_row(
            "SELECT MAX(expires_at) FROM spent_challenges WHERE expires_at <= ?1",
            params![now],
            |row| row.get(0),
        )
        .map_err(store_error)?;
    if let Some(cleared) = cleared.filter(|cleared| *cleared > horizon) {
        connection
            .execute(
                "INSERT INTO secrets (name, value) VALUES ('spent_horizon', ?1)
                 ON CONFLICT (name) DO UPDATE SET value = excluded.value",
                params![cleared.to_be_bytes().to_vec()],
            )
            .map_err(store_error)?;
        connection
            .execute(
                "DELETE FROM spent_challenges WHERE expires_at <= ?1",
                params![cleared],
            )
            .map_err(store_error)?;
    }
    let fresh = connection
        .execute(
            "INSERT OR IGNORE INTO spent_challenges (mac, expires_at) VALUES (?1, ?2)",
            params![mac, expires],
        )
        .map_err(store_error)?;
    if fresh == 1 {
        Ok(())
    } else {
        Err(invalid_ceremony())
    }
}

/// The challenge key the store holds: 32 bytes, or the store is damaged.
fn stored_key(connection: &Connection) -> BridgeResult<hmac::Key> {
    let key: Vec<u8> = connection
        .query_row(
            "SELECT value FROM secrets WHERE name = 'challenge_key'",
            [],
            |row| row.get(0),
        )
        .map_err(store_error)?;
    if key.len() != 32 {
        return Err(BridgeError::new(
            "account_store",
            "the challenge key in accounts.sqlite3 is not 32 bytes; restore the store from a backup",
        ));
    }
    Ok(hmac::Key::new(hmac::HMAC_SHA256, &key))
}

/// The latest expiry of the spent challenges cleared so far (0 before any). A value that is
/// not 8 bytes stops ceremonies rather than switching the protection off.
fn spent_horizon(connection: &Connection) -> BridgeResult<u64> {
    let stored: Option<Vec<u8>> = connection
        .query_row(
            "SELECT value FROM secrets WHERE name = 'spent_horizon'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_error)?;
    match stored {
        None => Ok(0),
        Some(bytes) => bytes.try_into().map(u64::from_be_bytes).map_err(|_| {
            BridgeError::new(
                "account_store",
                "the spent-challenge horizon in accounts.sqlite3 is damaged; restore the store from a backup",
            )
        }),
    }
}

/// The store and its -wal and -shm files, when present, readable by the hub alone.
fn make_private(path: &Path) -> BridgeResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let name = path.as_os_str().to_owned();
        for suffix in ["", "-wal", "-shm"] {
            let mut file = name.clone();
            file.push(suffix);
            match std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(BridgeError::new(
                        "account_store",
                        format!("{}: {error}", Path::new(&file).display()),
                    ));
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
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
