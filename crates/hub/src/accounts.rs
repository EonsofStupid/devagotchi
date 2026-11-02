//! The hub's own accounts (D25, §14): people sign in with passkeys on the hub itself, so a
//! hub works with nothing else deployed. Nomen ("Sign in with Shippin") is an extra way in.
//!
//! Passkey challenges are the hub's own and cost nothing to hand out: each is a time, a
//! random nonce and an HMAC over them, the ceremony's purpose and (to register) the
//! account, under a key kept in the store. Nothing is written until a ceremony succeeds;
//! then its challenge is recorded as spent, in the same transaction as what it did, until
//! it would have expired anyway. A flood of unanswered sign-ins fills nothing.
//!
//! The hub's pages use it in `web/passkeys.rs`; `devagotchi hub owner` makes the owner and
//! the one-time link.

pub mod claim;
pub mod oauth;
pub mod passkey;
pub mod sessions;

use aws_lc_rs::hmac;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use devagotchi_contracts::{BridgeError, BridgeResult};
use sha2::{Digest, Sha256};
use sqlx::{AnyConnection, Row, any::AnyRow};

use crate::{
    devices::now_secs,
    store::{Failed, Store, secs, store_error, transaction, unsecs},
};

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
#[allow(dead_code)] // the account page (32a-3) adds passkeys on a session's authority
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
    store: Store,
    /// How long the hub's own AI tokens live (`auth.tokens`).
    tokens: crate::config::TokenConfig,
    /// Signs the passkey challenges this process makes: the store's key as last read. The
    /// store's copy is the one that counts: every ceremony checks against it, read in its
    /// own transaction, and refreshes this one. It changes only when a clock is found set
    /// back (`clock_set_back`).
    challenge_key: std::sync::RwLock<hmac::Key>,
}

fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.trim().as_bytes()))
}

fn account_row(row: &AnyRow) -> Result<Account, sqlx::Error> {
    Ok(Account {
        id: row.try_get(0)?,
        name: row.try_get(1)?,
        owner: row.try_get::<i64, _>(2)? == 1,
        disabled_at: row.try_get::<Option<i64>, _>(3)?.map(unsecs),
    })
}

fn passkey_row(row: &AnyRow) -> Result<Passkey, sqlx::Error> {
    let alg: i64 = row.try_get(4)?;
    let count: i64 = row.try_get(5)?;
    let damaged = |what: &str| sqlx::Error::Decode(format!("passkey {what} out of range").into());
    Ok(Passkey {
        id: row.try_get(0)?,
        account: row.try_get(1)?,
        label: row.try_get(2)?,
        public_key: row.try_get(3)?,
        // Only accepted algorithms are ever stored; an unknown one reads as unusable.
        alg: Alg::from_cose(alg).ok_or_else(|| damaged("algorithm"))?,
        sign_count: u32::try_from(count).map_err(|_| damaged("counter"))?,
        backup_eligible: row.try_get::<i64, _>(6)? == 1,
    })
}

macro_rules! account_columns {
    () => {
        "id, name, owner, disabled_at"
    };
}
macro_rules! passkey_columns {
    () => {
        "id, account, label, public_key, alg, sign_count, backup_eligible"
    };
}

async fn account_in(connection: &mut AnyConnection, id: &str) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query(concat!("SELECT ", account_columns!(), " FROM accounts WHERE id = $1"))
        .bind(id)
        .fetch_optional(&mut *connection)
        .await?
        .as_ref()
        .map(account_row)
        .transpose()
}

async fn owner_in(connection: &mut AnyConnection) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query(concat!("SELECT ", account_columns!(), " FROM accounts WHERE owner = 1"))
        .fetch_optional(&mut *connection)
        .await?
        .as_ref()
        .map(account_row)
        .transpose()
}

impl Accounts {
    /// The accounts in `store`. Its challenge key is made the first time, once, whichever
    /// hub instance gets there first.
    pub async fn open(store: Store) -> BridgeResult<Self> {
        let fresh = crate::link::random::<32>().to_vec();
        let key = transaction!(&store, |connection| {
                sqlx::query(
                    "INSERT INTO secrets (name, value) VALUES ('challenge_key', $1)
                     ON CONFLICT (name) DO NOTHING",
                )
                .bind(&fresh)
                .execute(&mut *connection)
                .await?;
                Ok(stored_key(connection).await?)
            })?;
        Ok(Self {
            store,
            tokens: crate::config::TokenConfig::default(),
            challenge_key: std::sync::RwLock::new(key),
        })
    }

    /// These accounts with the config's lifetimes and limits for AI connections.
    pub fn with_tokens(mut self, tokens: crate::config::TokenConfig) -> Self {
        self.tokens = tokens;
        self
    }

    /// Make an account. There is at most one owner, and an id is never reused.
    pub async fn create(&self, id: &str, name: &str, owner: bool) -> BridgeResult<Account> {
        let name = check_name(id, name)?;
        let now = now_secs();
        transaction!(&self.store, |connection| {
                create_in(connection, id, name, owner, now).await
            })?;
        Ok(Account {
            id: id.to_string(),
            name: name.to_string(),
            owner,
            disabled_at: None,
        })
    }

    /// The hub's owner, made if there is none yet. `configured` is the config's `owner`, a
    /// subject from an earlier identity provider: the owner keeps it as the account id, so
    /// every board, machine and audit row keyed by it stays theirs. Without one, a new id.
    pub async fn ensure_owner(
        &self,
        configured: Option<&str>,
        name: &str,
        random: [u8; 16],
    ) -> BridgeResult<Account> {
        if let Some(owner) = self.owner().await? {
            return match configured {
                Some(id) if id != owner.id => Err(BridgeError::new(
                    "owner_mismatch",
                    format!(
                        "this hub's owner account is {}, not the config's owner {id}",
                        owner.id
                    ),
                )),
                _ => Ok(owner),
            };
        }
        let id = configured
            .map(str::to_string)
            .unwrap_or_else(|| new_account_id(random));
        self.create(&id, name, true).await
    }

    pub async fn get(&self, id: &str) -> BridgeResult<Option<Account>> {
        let mut connection = self.store.read().await?;
        account_in(&mut connection, id).await.map_err(store_error)
    }

    pub async fn owner(&self) -> BridgeResult<Option<Account>> {
        let mut connection = self.store.read().await?;
        owner_in(&mut connection).await.map_err(store_error)
    }

    /// Issue a one-time link for `account`: 256 random bits the store draws itself. Only
    /// its SHA-256 is kept; a new link of the same purpose supersedes the account's unused
    /// one.
    pub async fn issue_link(&self, purpose: LinkPurpose, account: &str) -> BridgeResult<String> {
        self.issue_link_at(purpose, account, crate::link::random::<32>(), now_secs())
            .await
    }

    /// Issue a one-time link for `account` whose use also removes every other passkey of the
    /// account (and so every session they opened): how its holder takes it back from a
    /// passkey someone else made with a leaked link (`devagotchi hub owner --replace`).
    pub async fn issue_replacing_link(
        &self,
        purpose: LinkPurpose,
        account: &str,
    ) -> BridgeResult<String> {
        self.issue(
            purpose,
            account,
            crate::link::random::<32>(),
            now_secs(),
            true,
        )
        .await
    }

    async fn issue_link_at(
        &self,
        purpose: LinkPurpose,
        account: &str,
        random: [u8; 32],
        now: u64,
    ) -> BridgeResult<String> {
        self.issue(purpose, account, random, now, false).await
    }

    async fn issue(
        &self,
        purpose: LinkPurpose,
        account: &str,
        random: [u8; 32],
        now: u64,
        replaces: bool,
    ) -> BridgeResult<String> {
        let token = link_token(random);
        transaction!(&self.store, |connection| {
                issue_in(connection, purpose, account, &token, now, replaces).await
            })?;
        Ok(token)
    }

    /// The account a link is for, and whether using it replaces the account's other
    /// passkeys, when it is unused and unexpired. Checking does not spend it.
    pub async fn check_link_replacing(
        &self,
        purpose: LinkPurpose,
        token: &str,
    ) -> BridgeResult<(String, bool)> {
        let mut connection = self.store.read().await?;
        live_link(&mut connection, purpose, token, now_secs())
            .await
            .map(|live| (live.account, live.replaces))
    }

    /// The account a link is for, when it is unused and unexpired. Checking does not
    /// spend it: a link preview cannot burn it.
    pub async fn check_link(&self, purpose: LinkPurpose, token: &str) -> BridgeResult<String> {
        self.check_link_at(purpose, token, now_secs()).await
    }

    pub(crate) async fn check_link_at(
        &self,
        purpose: LinkPurpose,
        token: &str,
        now: u64,
    ) -> BridgeResult<String> {
        let mut connection = self.store.read().await?;
        live_link(&mut connection, purpose, token, now)
            .await
            .map(|live| live.account)
    }

    /// Store a passkey for `account`, as `verify_registration` returned it, on the
    /// `authority` of a live link for this account. The challenge it answered must be one
    /// this hub made to register a passkey on this account. The challenge and the link are
    /// spent in the same transaction as the passkey is stored: all of it happens or none
    /// does.
    #[allow(dead_code)] // the account page (32a-3)
    pub async fn add_passkey(
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
        .await
    }

    /// `add_passkey`, with the authority optional: for this module and its tests only.
    async fn store_passkey(
        &self,
        account: &str,
        credential: &NewCredential,
        label: &str,
        link: Option<(LinkPurpose, &str)>,
        now: u64,
    ) -> BridgeResult<()> {
        self.put_passkey(account, credential, label, link, now, None)
            .await
            .map(|_| ())
    }

    /// Store a passkey and, with `session`, open the session it signs in to, in the same
    /// transaction as everything else: the link spent and the passkey stored together with
    /// the session, or none of it.
    pub(super) async fn put_passkey(
        &self,
        account: &str,
        credential: &NewCredential,
        label: &str,
        link: Option<(LinkPurpose, &str)>,
        now: u64,
        session: Option<[u8; 32]>,
    ) -> BridgeResult<Option<String>> {
        let label = label.trim();
        if label.is_empty() || label.len() > 64 || label.chars().any(char::is_control) {
            return Err(BridgeError::new(
                "invalid_passkey",
                "a passkey's name is 1 to 64 printable characters",
            ));
        }
        if self.clock_set_back(now).await? {
            return Err(invalid_ceremony());
        }
        transaction!(&self.store, |connection| {
                let (mac, issued) = self
                    .check_challenge(
                        connection,
                        Ceremony::Register,
                        Some(account),
                        &credential.challenge,
                        now,
                    )
                    .await?;
                spend_challenge(connection, &mac, issued, now).await?;
                let holder = account_in(connection, account)
                    .await?
                    .ok_or_else(|| BridgeError::new("no_account", format!("no account {account}")))?;
                if holder.disabled_at.is_some() {
                    return Err(account_disabled(account).into());
                }
                if let Some((purpose, token)) = link {
                    let live = live_link(connection, purpose, token, now).await?;
                    if live.account != account {
                        return Err(BridgeError::new(
                            "invalid_link",
                            "this link is for another account",
                        )
                        .into());
                    }
                    // A replacing link takes the account back: every other passkey goes, and
                    // with them every session they opened.
                    if live.replaces {
                        sqlx::query("DELETE FROM passkeys WHERE account = $1")
                            .bind(account)
                            .execute(&mut *connection)
                            .await?;
                    }
                    // Spent only if still unspent: two uses racing cannot both count.
                    let spent = sqlx::query(
                        "UPDATE links SET used_at = $1 WHERE hash = $2 AND used_at IS NULL",
                    )
                    .bind(secs(now))
                    .bind(hash(token))
                    .execute(&mut *connection)
                    .await?
                    .rows_affected();
                    if spent != 1 {
                        return Err(BridgeError::new(
                            "invalid_link",
                            "this link is used, expired or unknown",
                        )
                        .into());
                    }
                    // A link spent ends the account's other sessions: whoever had them, the
                    // passkey registered now is how the account signs in from here.
                    sqlx::query("DELETE FROM sessions WHERE account = $1")
                        .bind(account)
                        .execute(&mut *connection)
                        .await?;
                }
                let taken = sqlx::query("SELECT 1 FROM passkeys WHERE id = $1")
                    .bind(&credential.id)
                    .fetch_optional(&mut *connection)
                    .await?
                    .is_some();
                if taken {
                    return Err(BridgeError::new(
                        "passkey_exists",
                        "this passkey is already registered",
                    )
                    .into());
                }
                sqlx::query(
                    "INSERT INTO passkeys (id, account, label, public_key, alg, sign_count, backup_eligible, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(&credential.id)
                .bind(account)
                .bind(label)
                .bind(&credential.public_key)
                .bind(credential.alg.cose())
                .bind(i64::from(credential.sign_count))
                .bind(i64::from(credential.backup_eligible))
                .bind(secs(now))
                .execute(&mut *connection)
                .await?;
                // The owner can sign in now: the hub is claimed, and no claim code it
                // printed works any more (`claim.rs`).
                if holder.owner {
                    sqlx::query("DELETE FROM claim_codes")
                        .execute(&mut *connection)
                        .await?;
                }
                match session {
                    Some(random) => Ok(Some(
                        sessions::insert_session(
                            connection,
                            account,
                            Some(&credential.id),
                            random,
                            now,
                        )
                        .await?,
                    )),
                    None => Ok(None),
                }
            })
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
    /// transaction, so a key another hub instance replaced is never trusted here; this one
    /// takes it up for the challenges it makes next. Whether the challenge is spent is
    /// checked when it is.
    async fn check_challenge(
        &self,
        connection: &mut AnyConnection,
        purpose: Ceremony,
        account: Option<&str>,
        challenge: &[u8],
        now: u64,
    ) -> Result<(Vec<u8>, u64), Failed> {
        let key = stored_key(connection).await?;
        *self.challenge_key.write().expect("challenge key") = key.clone();
        if challenge.len() != CHALLENGE_LEN || challenge[0] != CHALLENGE_FORMAT {
            return Err(invalid_ceremony().into());
        }
        let (signed, tag) = challenge.split_at(CHALLENGE_LEN - 32);
        hmac::verify(&key, &challenge_message(signed, purpose, account), tag)
            .map_err(|_| invalid_ceremony())?;
        let issued = u64::from_be_bytes(signed[1..9].try_into().expect("8 bytes"));
        if issued > now || now >= issued.saturating_add(CHALLENGE_TTL_SECS) {
            return Err(invalid_ceremony().into());
        }
        Ok((tag.to_vec(), issued))
    }

    /// Whether the clock is now more than five minutes behind the horizon of cleared spent
    /// challenges: set back after running ahead. Then a challenge made before the step could
    /// be one whose record was cleared, and refusing all of them until the clock catches up
    /// could take hours. Instead the challenge key is replaced and the records and horizon
    /// start over, in one transaction that no ceremony can interleave with: every challenge
    /// made before fails once, every one made after works.
    async fn clock_set_back(&self, now: u64) -> BridgeResult<bool> {
        let key = crate::link::random::<32>();
        let replaced = transaction!(&self.store, |connection| {
                if now.saturating_add(CHALLENGE_TTL_SECS) > spent_horizon(connection).await? {
                    return Ok(false);
                }
                sqlx::query("UPDATE secrets SET value = $1 WHERE name = 'challenge_key'")
                    .bind(key.to_vec())
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("DELETE FROM secrets WHERE name = 'spent_horizon'")
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("DELETE FROM spent_challenges")
                    .execute(&mut *connection)
                    .await?;
                Ok(true)
            })?;
        if replaced {
            *self.challenge_key.write().expect("challenge key") =
                hmac::Key::new(hmac::HMAC_SHA256, &key);
            eprintln!(
                "devagotchi-hub: the clock was set back (now {now}); passkey challenges made before were withdrawn"
            );
        }
        Ok(replaced)
    }

    pub async fn passkey(&self, id: &[u8]) -> BridgeResult<Option<Passkey>> {
        let mut connection = self.store.read().await?;
        sqlx::query(concat!("SELECT ", passkey_columns!(), " FROM passkeys WHERE id = $1"))
            .bind(id)
            .fetch_optional(&mut *connection)
            .await
            .and_then(|row| row.as_ref().map(passkey_row).transpose())
            .map_err(store_error)
    }

    pub async fn passkeys_of(&self, account: &str) -> BridgeResult<Vec<Passkey>> {
        let mut connection = self.store.read().await?;
        sqlx::query(concat!(
            "SELECT ",
            passkey_columns!(),
            " FROM passkeys WHERE account = $1 ORDER BY created_at, id"
        ))
        .bind(account)
        .fetch_all(&mut *connection)
        .await
        .and_then(|rows| rows.iter().map(passkey_row).collect())
        .map_err(store_error)
    }

    /// Record a sign-in, as `check_sign_in` returned it. In one transaction: the challenge
    /// is a live sign-in challenge of this hub's and is spent; the passkey exists with the
    /// key that verified the signature; its account exists and is not disabled; the user
    /// handle is that account's; and the counter moves forward from what is stored now, so
    /// two sign-ins racing with one copied key cannot both count. Returns the account.
    pub async fn sign_in(&self, asserted: &Asserted) -> BridgeResult<Account> {
        self.sign_in_at(asserted, now_secs()).await
    }

    async fn sign_in_at(&self, asserted: &Asserted, now: u64) -> BridgeResult<Account> {
        if self.clock_set_back(now).await? {
            return Err(invalid_ceremony());
        }
        transaction!(&self.store, |connection| {
                let (mac, issued) = self
                    .check_challenge(connection, Ceremony::SignIn, None, &asserted.challenge, now)
                    .await?;
                spend_challenge(connection, &mac, issued, now).await?;
                let not_here =
                    || BridgeError::new("invalid_passkey", "this passkey is not registered here");
                let passkey = sqlx::query(concat!(
                    "SELECT ",
                    passkey_columns!(),
                    " FROM passkeys WHERE id = $1"
                ))
                .bind(&asserted.id)
                .fetch_optional(&mut *connection)
                .await?
                .as_ref()
                .map(passkey_row)
                .transpose()?
                .ok_or_else(not_here)?;
                // The key, algorithm and backup eligibility the signature was checked with
                // must be the ones stored for this passkey.
                if passkey.public_key != asserted.public_key
                    || passkey.alg != asserted.alg
                    || passkey.backup_eligible != asserted.backup_eligible
                {
                    return Err(not_here().into());
                }
                let account = account_in(connection, &passkey.account)
                    .await?
                    .ok_or_else(not_here)?;
                if account.disabled_at.is_some() {
                    return Err(account_disabled(&account.id).into());
                }
                if asserted.user_handle != account.id.as_bytes() {
                    return Err(BridgeError::new(
                        "user_handle",
                        "the passkey answered for another account",
                    )
                    .into());
                }
                // Authenticators that keep no counter (most synced passkeys) report 0 every
                // time.
                let count = asserted.sign_count;
                if (count != 0 || passkey.sign_count != 0) && count <= passkey.sign_count {
                    // A copy of the key is about: whoever holds the sessions this passkey
                    // opened may be the copy's holder, so they end. The challenge stays
                    // spent, so the same answer cannot do it again. This is committed, and
                    // the refusal returned after.
                    sqlx::query("DELETE FROM sessions WHERE passkey = $1")
                        .bind(&asserted.id)
                        .execute(&mut *connection)
                        .await?;
                    return Ok(Err(BridgeError::new(
                        "signature_counter",
                        "this passkey's counter went backwards: it may have been copied",
                    )));
                }
                sqlx::query("UPDATE passkeys SET sign_count = $1, last_used_at = $2 WHERE id = $3")
                    .bind(i64::from(count))
                    .bind(secs(now))
                    .bind(&asserted.id)
                    .execute(&mut *connection)
                    .await?;
                Ok(Ok(account))
            })?
    }

    /// Remove one of `account`'s passkeys; false when it has no such passkey. Its last one
    /// stays: removing it would lock the account out.
    #[allow(dead_code)] // the account page (32a-3)
    pub async fn remove_passkey(&self, account: &str, id: &[u8]) -> BridgeResult<bool> {
        transaction!(&self.store, |connection| {
                let held: i64 = sqlx::query("SELECT COUNT(*) FROM passkeys WHERE account = $1")
                    .bind(account)
                    .fetch_one(&mut *connection)
                    .await?
                    .try_get(0)?;
                let theirs = sqlx::query("SELECT 1 FROM passkeys WHERE id = $1 AND account = $2")
                    .bind(id)
                    .bind(account)
                    .fetch_optional(&mut *connection)
                    .await?
                    .is_some();
                if !theirs {
                    return Ok(false);
                }
                if held <= 1 {
                    return Err(BridgeError::new(
                        "last_passkey",
                        "this is the account's only passkey; add another before removing it",
                    )
                    .into());
                }
                sqlx::query("DELETE FROM passkeys WHERE id = $1 AND account = $2")
                    .bind(id)
                    .bind(account)
                    .execute(&mut *connection)
                    .await?;
                Ok(true)
            })
    }

    /// The store, for the modules that keep their tables beside the accounts.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }
}

fn account_disabled(account: &str) -> BridgeError {
    BridgeError::new("account_disabled", format!("{account} is disabled"))
}

/// An account id and name the hub accepts; the name trimmed.
fn check_name<'a>(id: &str, name: &'a str) -> BridgeResult<&'a str> {
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
    Ok(name)
}

/// `create`, inside a transaction the caller holds; `name` already checked.
async fn create_in(
    connection: &mut AnyConnection,
    id: &str,
    name: &str,
    owner: bool,
    now: u64,
) -> Result<(), Failed> {
    if account_in(connection, id).await?.is_some() {
        return Err(BridgeError::new("account_exists", format!("{id} already exists")).into());
    }
    if owner {
        let has_owner = sqlx::query("SELECT 1 FROM accounts WHERE owner = 1")
            .fetch_optional(&mut *connection)
            .await?
            .is_some();
        if has_owner {
            return Err(BridgeError::new("owner_exists", "this hub already has an owner").into());
        }
    }
    sqlx::query("INSERT INTO accounts (id, name, owner, created_at) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(name)
        .bind(i64::from(owner))
        .bind(secs(now))
        .execute(&mut *connection)
        .await?;
    Ok(())
}

/// A one-time link's token: `dgl_` and 256 random bits.
fn link_token(random: [u8; 32]) -> String {
    format!("dgl_{}", URL_SAFE_NO_PAD.encode(random))
}

/// Keep `token` as `account`'s live link for `purpose`, superseding its unused one, inside
/// a transaction the caller holds.
async fn issue_in(
    connection: &mut AnyConnection,
    purpose: LinkPurpose,
    account: &str,
    token: &str,
    now: u64,
    replaces: bool,
) -> Result<(), Failed> {
    let holder = account_in(connection, account)
        .await?
        .ok_or_else(|| BridgeError::new("no_account", format!("no account {account}")))?;
    if purpose == LinkPurpose::Owner && !holder.owner {
        return Err(BridgeError::new("not_owner", format!("{account} is not the owner")).into());
    }
    sqlx::query(
        "UPDATE links SET used_at = $1 WHERE purpose = $2 AND account = $3 AND used_at IS NULL",
    )
    .bind(secs(now))
    .bind(purpose.as_str())
    .bind(account)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO links (hash, purpose, account, expires_at, replaces)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(hash(token))
    .bind(purpose.as_str())
    .bind(account)
    .bind(secs(now + LINK_TTL_SECS))
    .bind(i64::from(replaces))
    .execute(&mut *connection)
    .await?;
    Ok(())
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
async fn spend_challenge(
    connection: &mut AnyConnection,
    mac: &[u8],
    issued: u64,
    now: u64,
) -> Result<(), Failed> {
    let expires = issued.saturating_add(CHALLENGE_TTL_SECS);
    let horizon = spent_horizon(connection).await?;
    if expires <= horizon {
        return Err(invalid_ceremony().into());
    }
    let cleared: Option<i64> =
        sqlx::query("SELECT MAX(expires_at) FROM spent_challenges WHERE expires_at <= $1")
            .bind(secs(now))
            .fetch_one(&mut *connection)
            .await?
            .try_get(0)?;
    if let Some(cleared) = cleared.map(unsecs).filter(|cleared| *cleared > horizon) {
        sqlx::query(
            "INSERT INTO secrets (name, value) VALUES ('spent_horizon', $1)
             ON CONFLICT (name) DO UPDATE SET value = excluded.value",
        )
        .bind(cleared.to_be_bytes().to_vec())
        .execute(&mut *connection)
        .await?;
        sqlx::query("DELETE FROM spent_challenges WHERE expires_at <= $1")
            .bind(secs(cleared))
            .execute(&mut *connection)
            .await?;
    }
    let fresh = sqlx::query(
        "INSERT INTO spent_challenges (mac, expires_at) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(mac)
    .bind(secs(expires))
    .execute(&mut *connection)
    .await?
    .rows_affected();
    if fresh == 1 {
        Ok(())
    } else {
        Err(invalid_ceremony().into())
    }
}

/// The challenge key the store holds: 32 bytes, or the store is damaged.
async fn stored_key(connection: &mut AnyConnection) -> Result<hmac::Key, Failed> {
    let key: Vec<u8> = sqlx::query("SELECT value FROM secrets WHERE name = 'challenge_key'")
        .fetch_one(&mut *connection)
        .await?
        .try_get(0)?;
    if key.len() != 32 {
        return Err(BridgeError::new(
            "account_store",
            "the challenge key in the store is not 32 bytes; restore the store from a backup",
        )
        .into());
    }
    Ok(hmac::Key::new(hmac::HMAC_SHA256, &key))
}

/// The latest expiry of the spent challenges cleared so far (0 before any). A value that is
/// not 8 bytes stops ceremonies rather than switching the protection off.
async fn spent_horizon(connection: &mut AnyConnection) -> Result<u64, Failed> {
    let stored: Option<Vec<u8>> = sqlx::query("SELECT value FROM secrets WHERE name = 'spent_horizon'")
        .fetch_optional(&mut *connection)
        .await?
        .map(|row| row.try_get(0))
        .transpose()?;
    match stored {
        None => Ok(0),
        Some(bytes) => bytes.try_into().map(u64::from_be_bytes).map_err(|_| {
            BridgeError::new(
                "account_store",
                "the spent-challenge horizon in the store is damaged; restore the store from a backup",
            )
            .into()
        }),
    }
}

/// A link that is unused and unexpired: the account it is for, and whether using it
/// replaces the account's other passkeys.
struct LiveLink {
    account: String,
    replaces: bool,
}

async fn live_link(
    connection: &mut AnyConnection,
    purpose: LinkPurpose,
    token: &str,
    now: u64,
) -> BridgeResult<LiveLink> {
    let invalid = || BridgeError::new("invalid_link", "this link is used, expired or unknown");
    let row = sqlx::query(
        "SELECT account, expires_at, used_at, replaces FROM links WHERE hash = $1 AND purpose = $2",
    )
    .bind(hash(token))
    .bind(purpose.as_str())
    .fetch_optional(&mut *connection)
    .await
    .map_err(store_error)?;
    let Some(row) = row else {
        return Err(invalid());
    };
    let decode = |row: &AnyRow| -> Result<(String, i64, Option<i64>, i64), sqlx::Error> {
        Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?, row.try_get(3)?))
    };
    match decode(&row).map_err(store_error)? {
        (account, expires_at, None, replaces) if secs(now) < expires_at => Ok(LiveLink {
            account,
            replaces: replaces == 1,
        }),
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests;
