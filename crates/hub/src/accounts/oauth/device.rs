//! Device authorization (RFC 8628) for the hub's own accounts: how `devagotchi login`
//! signs a terminal in. The terminal asks for a code, the person opens `/device` on any
//! device where they are signed in with their passkey, checks the code matches and
//! approves, and the terminal's next poll gets an access and a refresh token like any AI
//! client's: for this hub's `/mcp`, in their own family, dying with the account.
//!
//! - The device code (`dgd_`, 256 random bits) and the user code are kept only as SHA-256.
//! - The user code is 8 letters from 20 consonants (RFC 8628 §6.1: no vowels, so no words;
//!   about 34 bits), typed in any case, with or without its dash.
//! - A grant waits `DEVICE_SECS` at most; decided or expired, it is gone after one poll.
//! - Polling faster than `DEVICE_INTERVAL` is answered `slow_down`.
//! - At most `MAX_PENDING` grants wait at once, so a flood of requests fills nothing.

use super::*;

/// How long a device code may wait for the person.
pub const DEVICE_SECS: u64 = 10 * 60;
/// Seconds between polls the terminal is told to keep (RFC 8628 §3.2's default).
pub const DEVICE_INTERVAL: u64 = 5;
/// Grants waiting at once; past it, new requests are refused until some end.
const MAX_PENDING: i64 = 1000;
/// RFC 8628 §6.1's example alphabet: consonants only.
const ALPHABET: &[u8; 20] = b"BCDFGHJKLMNPQRSTVWXZ";

/// What the terminal is told: the codes, and how long and how often to poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStart {
    pub device_code: String,
    /// As the person reads it: `XXXX-XXXX`.
    pub user_code: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// A grant waiting for the person, as `/device` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePending {
    pub client: String,
    pub scope: String,
}

/// What a poll finds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePoll {
    Tokens(Tokens),
    /// RFC 8628 §3.5's error codes: `authorization_pending`, `slow_down`, `access_denied`,
    /// `expired_token`; and `invalid_grant` for a code that is unknown or not this client's.
    Waiting(&'static str),
}

/// The user code a person typed, as stored: 8 letters of the alphabet, upper case, no
/// dash. `None` for anything else.
pub fn normalize_user_code(typed: &str) -> Option<String> {
    let letters: String = typed
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (letters.len() == 8 && letters.bytes().all(|b| ALPHABET.contains(&b))).then_some(letters)
}

pub(super) fn new_user_code() -> String {
    let mut code = String::with_capacity(9);
    while code.len() < 9 {
        // 240 is the largest multiple of 20 a byte holds: no letter is likelier.
        for byte in crate::link::random::<16>() {
            if byte < 240 && code.len() < 9 {
                if code.len() == 4 {
                    code.push('-');
                }
                code.push(ALPHABET[usize::from(byte % 20)] as char);
            }
        }
    }
    code
}

impl Accounts {
    /// Start a device grant for `client` (RFC 8628 §3.1).
    pub async fn start_device(
        &self,
        client: &str,
        scope: &str,
        resource: &str,
    ) -> BridgeResult<DeviceStart> {
        self.start_device_at(client, scope, resource, now_secs())
            .await
    }

    pub(crate) async fn start_device_at(
        &self,
        client: &str,
        scope: &str,
        resource: &str,
        now: u64,
    ) -> BridgeResult<DeviceStart> {
        let device_code = format!(
            "dgd_{}",
            URL_SAFE_NO_PAD.encode(crate::link::random::<32>())
        );
        let user_code = new_user_code();
        let stored = normalize_user_code(&user_code).expect("a code this hub made");
        transaction!(&self.store, |connection| {
            sqlx::query("DELETE FROM oauth_devices WHERE expires_at <= $1")
                .bind(secs(now))
                .execute(&mut *connection)
                .await?;
            let waiting: i64 = sqlx::query("SELECT COUNT(*) FROM oauth_devices")
                .fetch_one(&mut *connection)
                .await?
                .try_get(0)?;
            if waiting >= MAX_PENDING {
                return Ok(Err(BridgeError::new(
                    "slow_down",
                    "too many sign-ins are waiting; try again in a minute",
                )));
            }
            sqlx::query(
                "INSERT INTO oauth_devices (hash, user_code, client, scope, resource, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(hash(&device_code))
            .bind(hash(&stored))
            .bind(client)
            .bind(scope)
            .bind(resource)
            .bind(secs(now.saturating_add(DEVICE_SECS)))
            .execute(&mut *connection)
            .await?;
            Ok(Ok(()))
        })??;
        Ok(DeviceStart {
            device_code,
            user_code,
            expires_in: DEVICE_SECS,
            interval: DEVICE_INTERVAL,
        })
    }

    /// The grant a person's code names, while it waits for them.
    pub async fn pending_device(&self, user_code: &str) -> BridgeResult<Option<DevicePending>> {
        let Some(code) = normalize_user_code(user_code) else {
            return Ok(None);
        };
        let mut connection = self.store.read().await?;
        sqlx::query(
            "SELECT client, scope FROM oauth_devices
             WHERE user_code = $1 AND expires_at > $2 AND account IS NULL AND denied = 0",
        )
        .bind(hash(&code))
        .bind(secs(now_secs()))
        .fetch_optional(&mut *connection)
        .await
        .and_then(|row| {
            row.map(|row| -> Result<DevicePending, sqlx::Error> {
                Ok(DevicePending {
                    client: row.try_get(0)?,
                    scope: row.try_get(1)?,
                })
            })
            .transpose()
        })
        .map_err(store_error)
    }

    /// The person's answer to the grant their code names: approved for `account`, or
    /// denied. `false` when no grant waits under that code (unknown, expired, decided).
    pub async fn decide_device(
        &self,
        user_code: &str,
        account: &str,
        approve: bool,
    ) -> BridgeResult<bool> {
        let Some(code) = normalize_user_code(user_code) else {
            return Ok(false);
        };
        let now = now_secs();
        transaction!(&self.store, |connection| {
            if approve {
                // An account nobody secured connects only so many apps and terminals.
                self.enter(connection, crate::store::Lookup::Account(account))
                    .await?;
                self.room_for_a_client(connection, account, now).await?;
            }
            let decided = if approve {
                sqlx::query(
                    "UPDATE oauth_devices SET account = $1
                     WHERE user_code = $2 AND expires_at > $3 AND account IS NULL AND denied = 0",
                )
                .bind(account)
                .bind(hash(&code))
                .bind(secs(now))
            } else {
                sqlx::query(
                    "UPDATE oauth_devices SET denied = 1
                     WHERE user_code = $1 AND expires_at > $2 AND account IS NULL AND denied = 0",
                )
                .bind(hash(&code))
                .bind(secs(now))
            }
            .execute(&mut *connection)
            .await?
            .rows_affected();
            Ok(decided == 1)
        })
    }

    /// The terminal's poll (RFC 8628 §3.4): tokens once the person approved, and the grant
    /// is gone; otherwise why not yet, or not at all.
    pub async fn poll_device(&self, device_code: &str, client: &str) -> BridgeResult<DevicePoll> {
        self.poll_device_at(device_code, client, now_secs()).await
    }

    pub(crate) async fn poll_device_at(
        &self,
        device_code: &str,
        client: &str,
        now: u64,
    ) -> BridgeResult<DevicePoll> {
        let family = URL_SAFE_NO_PAD.encode(crate::link::random::<16>());
        let fresh = (crate::link::random::<32>(), crate::link::random::<32>());
        transaction!(&self.store, |connection| {
            let row = sqlx::query(
                "SELECT client, scope, resource, account, denied, polled_at, expires_at
                 FROM oauth_devices WHERE hash = $1",
            )
            .bind(hash(device_code))
            .fetch_optional(&mut *connection)
            .await?;
            let Some(row) = row else {
                return Ok(Ok(DevicePoll::Waiting("invalid_grant")));
            };
            let owner: String = row.try_get(0)?;
            let scope: String = row.try_get(1)?;
            let resource: String = row.try_get(2)?;
            let account: Option<String> = row.try_get(3)?;
            let denied = row.try_get::<i64, _>(4)? == 1;
            let polled_at = row.try_get::<Option<i64>, _>(5)?.map(unsecs);
            let expires_at = unsecs(row.try_get(6)?);
            if owner != client {
                return Ok(Ok(DevicePoll::Waiting("invalid_grant")));
            }
            // Expired, denied or approved: this poll is the grant's last.
            if now >= expires_at || denied || account.is_some() {
                sqlx::query("DELETE FROM oauth_devices WHERE hash = $1")
                    .bind(hash(device_code))
                    .execute(&mut *connection)
                    .await?;
            }
            if now >= expires_at {
                return Ok(Ok(DevicePoll::Waiting("expired_token")));
            }
            if denied {
                return Ok(Ok(DevicePoll::Waiting("access_denied")));
            }
            let Some(account) = account else {
                // One second of slack for a terminal that sleeps exactly the interval.
                let early =
                    polled_at.is_some_and(|at| now.saturating_add(1) < at + DEVICE_INTERVAL);
                sqlx::query("UPDATE oauth_devices SET polled_at = $1 WHERE hash = $2")
                    .bind(secs(now))
                    .bind(hash(device_code))
                    .execute(&mut *connection)
                    .await?;
                return Ok(Ok(DevicePoll::Waiting(if early {
                    "slow_down"
                } else {
                    "authorization_pending"
                })));
            };
            let tenant = self
                .enter(connection, crate::store::Lookup::Account(&account))
                .await?
                .unwrap_or_default();
            if let Err(refused) = self.live_account(connection, &account, now).await? {
                return Ok(Err(refused));
            }
            let tokens = self
                .mint(
                    connection, &tenant, &family, fresh, client, &account, &scope, &resource, now,
                )
                .await?;
            Ok(Ok(DevicePoll::Tokens(tokens)))
        })?
    }
}
