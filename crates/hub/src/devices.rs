//! Machines enrolled to reach this hub over the link (plan N20). A device is a name
//! bound to one Ed25519 public key. Binding happens once, with a one-time code; the code
//! itself is never stored, only its SHA-256. The hub operator issues codes for the
//! brokers in the hub config; a signed-in account issues them for machines of its own,
//! which only that account reaches (owned machines, increment 29a). People's phones and
//! browsers that receive approval notifications (Web Push) are kept here too.

use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use devagotchi_contracts::{BridgeError, BridgeResult};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

/// How long an enrollment code stays valid.
pub const CODE_TTL_SECS: u64 = 15 * 60;

/// How many machines an account may hold on a hub it does not own: joined and not
/// revoked, plus names it holds an unused code for.
pub const OWNED_LIMIT: usize = 16;

pub struct Devices {
    connection: Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Device {
    pub name: String,
    pub public_key: String,
    pub enrolled_at: u64,
    pub revoked_at: Option<u64>,
    /// The account that added this machine with a code of its own; `None` for a broker
    /// the hub operator configured.
    pub owner: Option<String>,
}

fn store_error(error: rusqlite::Error) -> BridgeError {
    BridgeError::new("device_store", error.to_string())
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Mark every unused code for `device` as spent.
fn cancel_codes(
    transaction: &rusqlite::Transaction<'_>,
    device: &str,
    now: u64,
) -> BridgeResult<usize> {
    transaction
        .execute(
            "UPDATE enroll_codes SET used_at = ?1 WHERE device = ?2 AND used_at IS NULL",
            params![now, device],
        )
        .map_err(store_error)
}

fn name_taken(device: &str) -> BridgeError {
    BridgeError::new(
        "name_taken",
        format!("{device:?} is already in use on this hub; choose another name"),
    )
}

/// Add `owner` to a table made before machines had owners.
fn add_owner_column(connection: &Connection, table: &str) -> rusqlite::Result<()> {
    let has: bool = connection
        .prepare(&format!(
            "SELECT 1 FROM pragma_table_info('{table}') WHERE name = 'owner'"
        ))?
        .exists([])?;
    if !has {
        connection.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN owner TEXT"))?;
    }
    Ok(())
}

const DEVICE_COLUMNS: &str = "name, public_key, enrolled_at, revoked_at, owner";

fn device_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    Ok(Device {
        name: row.get(0)?,
        public_key: row.get(1)?,
        enrolled_at: row.get(2)?,
        revoked_at: row.get(3)?,
        owner: row.get(4)?,
    })
}

fn code_hash(code: &str) -> String {
    hex::encode(Sha256::digest(code.trim().as_bytes()))
}

/// A phone or browser a person turned notifications on from (Web Push). The keys are the
/// browser's public half; the hub cannot read anything with them, only encrypt to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSubscription {
    pub endpoint: String,
    pub subject: String,
    pub p256dh: String,
    pub auth: String,
}

impl Devices {
    /// Remember a push subscription for `subject`; the same endpoint again replaces it.
    pub fn add_push(
        &self,
        subject: &str,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
    ) -> BridgeResult<()> {
        self.connection
            .lock()
            .expect("devices")
            .execute(
                "INSERT INTO push_subscriptions (endpoint, subject, p256dh, auth, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(endpoint) DO UPDATE SET subject = ?2, p256dh = ?3, auth = ?4",
                params![endpoint, subject, p256dh, auth, now_secs() as i64],
            )
            .map_err(store_error)?;
        Ok(())
    }

    pub fn push_for(&self, subject: &str) -> BridgeResult<Vec<PushSubscription>> {
        let connection = self.connection.lock().expect("devices");
        let mut statement = connection
            .prepare(
                "SELECT endpoint, subject, p256dh, auth FROM push_subscriptions WHERE subject = ?1",
            )
            .map_err(store_error)?;
        let rows = statement
            .query_map(params![subject], |row| {
                Ok(PushSubscription {
                    endpoint: row.get(0)?,
                    subject: row.get(1)?,
                    p256dh: row.get(2)?,
                    auth: row.get(3)?,
                })
            })
            .map_err(store_error)?;
        rows.collect::<Result<_, _>>().map_err(store_error)
    }

    pub fn drop_push(&self, endpoint: &str) -> BridgeResult<()> {
        self.connection
            .lock()
            .expect("devices")
            .execute(
                "DELETE FROM push_subscriptions WHERE endpoint = ?1",
                params![endpoint],
            )
            .map_err(store_error)?;
        Ok(())
    }

    pub fn open(path: &Path) -> BridgeResult<Self> {
        let connection = Connection::open(path).map_err(store_error)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = FULL;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE IF NOT EXISTS devices (
                   name TEXT PRIMARY KEY,
                   public_key TEXT NOT NULL,
                   enrolled_at INTEGER NOT NULL,
                   revoked_at INTEGER
                 );
                 CREATE TABLE IF NOT EXISTS enroll_codes (
                   code_hash TEXT PRIMARY KEY,
                   device TEXT NOT NULL,
                   expires_at INTEGER NOT NULL,
                   used_at INTEGER
                 );
                 CREATE TABLE IF NOT EXISTS push_subscriptions (
                   endpoint TEXT PRIMARY KEY,
                   subject TEXT NOT NULL,
                   p256dh TEXT NOT NULL,
                   auth TEXT NOT NULL,
                   created_at INTEGER NOT NULL
                 );",
            )
            .map_err(store_error)?;
        add_owner_column(&connection, "devices").map_err(store_error)?;
        add_owner_column(&connection, "enroll_codes").map_err(store_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Issue a one-time code that lets one machine bind its key to `device`. A new code
    /// supersedes every unused one for the same device, so at most one is ever live.
    pub fn issue_code(&self, device: &str, random: [u8; 16]) -> BridgeResult<String> {
        let code = format!("dvg-{}", hex::encode(random));
        let now = now_secs();
        let mut connection = self.connection.lock().expect("device lock");
        let transaction = connection.transaction().map_err(store_error)?;
        cancel_codes(&transaction, device, now)?;
        transaction
            .execute(
                "INSERT INTO enroll_codes (code_hash, device, expires_at) VALUES (?1, ?2, ?3)",
                params![code_hash(&code), device, now + CODE_TTL_SECS],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        Ok(code)
    }

    /// Issue a one-time code that lets one machine join as `device`, owned by `owner`:
    /// once it binds, `owner` reaches it and nobody else does. The name must be free on
    /// this hub, or already this owner's (a new code re-adds the machine with a new key).
    /// The caller refuses names the hub config uses. A new code supersedes the owner's
    /// unused one for the name; an unused, unexpired code another account holds keeps the
    /// name for that account until it is spent or expires.
    pub fn issue_owned_code(
        &self,
        owner: &str,
        device: &str,
        random: [u8; 16],
        limit: usize,
    ) -> BridgeResult<String> {
        let code = format!("dvg-{}", hex::encode(random));
        let now = now_secs();
        let mut connection = self.connection.lock().expect("device lock");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let holder: Option<(Option<String>, Option<u64>)> = transaction
            .query_row(
                "SELECT owner, revoked_at FROM devices WHERE name = ?1",
                params![device],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(store_error)?;
        if holder
            .as_ref()
            .is_some_and(|(holder, _)| holder.as_deref() != Some(owner))
        {
            return Err(name_taken(device));
        }
        // A machine it holds and has not revoked takes no new place; a revoked one does.
        let new_place = !matches!(holder, Some((_, None)));
        let pending_elsewhere: bool = transaction
            .prepare(
                "SELECT 1 FROM enroll_codes WHERE device = ?1 AND used_at IS NULL
                   AND expires_at >= ?2 AND (owner IS NULL OR owner != ?3)",
            )
            .and_then(|mut statement| statement.exists(params![device, now, owner]))
            .map_err(store_error)?;
        if pending_elsewhere {
            return Err(name_taken(device));
        }
        if new_place {
            let held: i64 = transaction
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM devices WHERE owner = ?1 AND revoked_at IS NULL)
                          + (SELECT COUNT(DISTINCT device) FROM enroll_codes
                               WHERE owner = ?1 AND used_at IS NULL AND expires_at >= ?2
                                 AND device != ?3
                                 AND device NOT IN (SELECT name FROM devices WHERE owner = ?1))",
                    params![owner, now, device],
                    |row| row.get(0),
                )
                .map_err(store_error)?;
            if held as usize >= limit {
                return Err(BridgeError::new(
                    "machine_limit",
                    format!(
                        "this account already has {held} machines on this hub (the most is {limit}); revoke one first"
                    ),
                ));
            }
        }
        cancel_codes(&transaction, device, now)?;
        transaction
            .execute(
                "INSERT INTO enroll_codes (code_hash, device, expires_at, owner) VALUES (?1, ?2, ?3, ?4)",
                params![code_hash(&code), device, now + CODE_TTL_SECS, owner],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        Ok(code)
    }

    /// Spend `code` for `device` and bind `public_key` to it. Re-enrolling a name with a
    /// fresh code replaces its key and clears a revocation: a new code is the operator's
    /// or the owner's act. A machine the hub config does not name (`configured` false)
    /// joins only with a code an account issued; it then belongs to that account.
    pub fn enroll(
        &self,
        device: &str,
        code: &str,
        public_key: &str,
        configured: bool,
    ) -> BridgeResult<Device> {
        let refused = |message: &str| BridgeError::new("enroll_refused", message);
        let mut connection = self.connection.lock().expect("device lock");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let row: Option<(String, u64, Option<u64>, Option<String>)> = transaction
            .query_row(
                "SELECT device, expires_at, used_at, owner FROM enroll_codes WHERE code_hash = ?1",
                params![code_hash(code)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(store_error)?;
        let now = now_secs();
        let owner = match row {
            None => return Err(refused("unknown enrollment code")),
            Some((bound, ..)) if bound != device => {
                return Err(refused("this code was issued for another device"));
            }
            Some((_, _, Some(_), _)) => return Err(refused("this code was already used")),
            Some((_, expires, None, _)) if expires < now => {
                return Err(refused("this code has expired"));
            }
            Some((.., owner)) => owner,
        };
        if owner.is_none() && !configured {
            return Err(refused(
                "this hub's configuration does not name this device",
            ));
        }
        // Spend this code and any other unused one for the device: a binding ends them all.
        cancel_codes(&transaction, device, now)?;
        transaction
            .execute(
                "INSERT INTO devices (name, public_key, enrolled_at, revoked_at, owner) VALUES (?1, ?2, ?3, NULL, ?4)
                 ON CONFLICT(name) DO UPDATE SET public_key = excluded.public_key,
                   enrolled_at = excluded.enrolled_at, revoked_at = NULL, owner = excluded.owner",
                params![device, public_key, now, owner],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        Ok(Device {
            name: device.to_string(),
            public_key: public_key.to_string(),
            enrolled_at: now,
            revoked_at: None,
            owner,
        })
    }

    /// The machines `owner` added, by name, revoked ones included.
    pub fn owned_by(&self, owner: &str) -> BridgeResult<Vec<Device>> {
        let connection = self.connection.lock().expect("device lock");
        let mut statement = connection
            .prepare(&format!(
                "SELECT {DEVICE_COLUMNS} FROM devices WHERE owner = ?1 ORDER BY name"
            ))
            .map_err(store_error)?;
        let rows = statement
            .query_map(params![owner], device_row)
            .map_err(store_error)?;
        rows.collect::<Result<_, _>>().map_err(store_error)
    }

    /// Revoke one of `owner`'s machines and cancel its unused codes (also a code still
    /// pending for a name that never joined). Another account's machine, or a configured
    /// one, answers `not_found`, so names of other accounts' machines are not revealed.
    pub fn revoke_owned(&self, owner: &str, device: &str) -> BridgeResult<(bool, usize)> {
        let now = now_secs();
        let mut connection = self.connection.lock().expect("device lock");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let holder: Option<Option<String>> = transaction
            .query_row(
                "SELECT owner FROM devices WHERE name = ?1",
                params![device],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_error)?;
        let pending_mine: bool = transaction
            .prepare(
                "SELECT 1 FROM enroll_codes WHERE device = ?1 AND used_at IS NULL AND owner = ?2",
            )
            .and_then(|mut statement| statement.exists(params![device, owner]))
            .map_err(store_error)?;
        let mine = match holder {
            Some(holder) => holder.as_deref() == Some(owner),
            None => pending_mine,
        };
        if !mine {
            return Err(BridgeError::new(
                "not_found",
                format!("you have no machine named {device:?}"),
            ));
        }
        let cancelled = cancel_codes(&transaction, device, now)?;
        let revoked = transaction
            .execute(
                "UPDATE devices SET revoked_at = ?1 WHERE name = ?2 AND revoked_at IS NULL",
                params![now, device],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        Ok((revoked == 1, cancelled))
    }

    pub fn get(&self, device: &str) -> BridgeResult<Option<Device>> {
        let connection = self.connection.lock().expect("device lock");
        connection
            .query_row(
                &format!("SELECT {DEVICE_COLUMNS} FROM devices WHERE name = ?1"),
                params![device],
                device_row,
            )
            .optional()
            .map_err(store_error)
    }

    pub fn is_revoked(&self, device: &str) -> BridgeResult<bool> {
        Ok(self.get(device)?.is_none_or(|d| d.revoked_at.is_some()))
    }

    /// Revoke a device and cancel every unused code for it. Its open link closes within
    /// seconds and nothing can enroll under the name until the operator issues a new code.
    /// Returns whether an active device was revoked and how many codes were cancelled.
    pub fn revoke(&self, device: &str) -> BridgeResult<(bool, usize)> {
        let now = now_secs();
        let mut connection = self.connection.lock().expect("device lock");
        let transaction = connection.transaction().map_err(store_error)?;
        let cancelled = cancel_codes(&transaction, device, now)?;
        let revoked = transaction
            .execute(
                "UPDATE devices SET revoked_at = ?1 WHERE name = ?2 AND revoked_at IS NULL",
                params![now, device],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        Ok((revoked == 1, cancelled))
    }

    pub fn list(&self) -> BridgeResult<Vec<Device>> {
        let connection = self.connection.lock().expect("device lock");
        let mut statement = connection
            .prepare(&format!(
                "SELECT {DEVICE_COLUMNS} FROM devices ORDER BY name"
            ))
            .map_err(store_error)?;
        let rows = statement.query_map([], device_row).map_err(store_error)?;
        rows.collect::<Result<_, _>>().map_err(store_error)
    }

    #[cfg(test)]
    fn expire_codes(&self) {
        let connection = self.connection.lock().unwrap();
        connection
            .execute("UPDATE enroll_codes SET expires_at = 0", [])
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Devices) {
        let dir = tempfile::tempdir().unwrap();
        let devices = Devices::open(&dir.path().join("devices.sqlite3")).unwrap();
        (dir, devices)
    }

    #[test]
    fn a_code_binds_one_key_once_and_only_for_its_device() {
        let (_dir, devices) = store();
        let code = devices.issue_code("gus", [7; 16]).unwrap();
        assert!(code.starts_with("dvg-") && code.len() == 36);
        assert_eq!(
            devices.enroll("dabo", &code, "aa", true).unwrap_err().code,
            "enroll_refused"
        );
        devices.enroll("gus", &code, "aa", true).unwrap();
        assert_eq!(devices.get("gus").unwrap().unwrap().public_key, "aa");
        assert_eq!(
            devices.enroll("gus", &code, "bb", true).unwrap_err().code,
            "enroll_refused"
        );
        assert_eq!(devices.get("gus").unwrap().unwrap().public_key, "aa");
    }

    #[test]
    fn only_the_newest_code_lives_and_binding_or_revoking_ends_it() {
        let (_dir, devices) = store();
        let older = devices.issue_code("gus", [10; 16]).unwrap();
        let newer = devices.issue_code("gus", [11; 16]).unwrap();
        assert!(
            devices.enroll("gus", &older, "aa", true).is_err(),
            "a superseded code must not work"
        );
        let spare = devices.issue_code("dabo", [12; 16]).unwrap();
        devices.enroll("gus", &newer, "aa", true).unwrap();
        // A code for another device is untouched by gus's binding.
        let pending = devices.issue_code("gus", [13; 16]).unwrap();
        assert_eq!(devices.revoke("gus").unwrap(), (true, 1));
        assert!(
            devices.enroll("gus", &pending, "bb", true).is_err(),
            "revoke must cancel pending codes"
        );
        assert_eq!(devices.get("gus").unwrap().unwrap().public_key, "aa");
        devices.enroll("dabo", &spare, "cc", true).unwrap();
        // Revoking a name that never enrolled still cancels its codes.
        let orphan = devices.issue_code("solo", [14; 16]).unwrap();
        assert_eq!(devices.revoke("solo").unwrap(), (false, 1));
        assert!(devices.enroll("solo", &orphan, "dd", true).is_err());
    }

    #[test]
    fn expired_and_unknown_codes_are_refused() {
        let (_dir, devices) = store();
        let code = devices.issue_code("gus", [1; 16]).unwrap();
        devices.expire_codes();
        assert!(devices.enroll("gus", &code, "aa", true).is_err());
        assert!(devices.enroll("gus", "dvg-nope", "aa", true).is_err());
        assert!(devices.get("gus").unwrap().is_none());
    }

    #[test]
    fn revoke_sticks_until_a_new_code_re_enrolls() {
        let (_dir, devices) = store();
        let first = devices.issue_code("gus", [2; 16]).unwrap();
        devices.enroll("gus", &first, "aa", true).unwrap();
        assert!(!devices.is_revoked("gus").unwrap());
        assert_eq!(devices.revoke("gus").unwrap(), (true, 0));
        assert!(devices.is_revoked("gus").unwrap());
        assert_eq!(devices.revoke("gus").unwrap(), (false, 0));
        let second = devices.issue_code("gus", [3; 16]).unwrap();
        devices.enroll("gus", &second, "cc", true).unwrap();
        assert!(!devices.is_revoked("gus").unwrap());
        assert_eq!(devices.list().unwrap().len(), 1);
        assert!(devices.is_revoked("never-enrolled").unwrap());
    }

    #[test]
    fn an_owned_code_binds_a_machine_to_its_account_only() {
        let (_dir, devices) = store();
        let code = devices
            .issue_owned_code("acct-a", "platinum", [20; 16], OWNED_LIMIT)
            .unwrap();
        // Not in the hub config, yet it joins: the code says whose it is.
        let device = devices.enroll("platinum", &code, "aa", false).unwrap();
        assert_eq!(device.owner.as_deref(), Some("acct-a"));
        assert_eq!(
            devices.get("platinum").unwrap().unwrap().owner.as_deref(),
            Some("acct-a")
        );
        assert_eq!(devices.owned_by("acct-a").unwrap().len(), 1);
        assert!(devices.owned_by("acct-b").unwrap().is_empty());
        // Another account cannot take the name, nor revoke it, nor learn more than "not yours".
        assert_eq!(
            devices
                .issue_owned_code("acct-b", "platinum", [21; 16], OWNED_LIMIT)
                .unwrap_err()
                .code,
            "name_taken"
        );
        assert_eq!(
            devices.revoke_owned("acct-b", "platinum").unwrap_err().code,
            "not_found"
        );
        // The owner re-adds it with a new key after revoking it.
        assert_eq!(
            devices.revoke_owned("acct-a", "platinum").unwrap(),
            (true, 0)
        );
        assert!(devices.is_revoked("platinum").unwrap());
        let again = devices
            .issue_owned_code("acct-a", "platinum", [22; 16], OWNED_LIMIT)
            .unwrap();
        devices.enroll("platinum", &again, "bb", false).unwrap();
        let record = devices.get("platinum").unwrap().unwrap();
        assert_eq!(
            (record.public_key.as_str(), record.revoked_at),
            ("bb", None)
        );
    }

    #[test]
    fn operator_codes_and_configured_names_stay_the_operators() {
        let (_dir, devices) = store();
        // An operator's code joins only a device the hub config names.
        let code = devices.issue_code("gus", [30; 16]).unwrap();
        assert_eq!(
            devices.enroll("gus", &code, "aa", false).unwrap_err().code,
            "enroll_refused"
        );
        let code = devices.issue_code("gus", [31; 16]).unwrap();
        assert_eq!(
            devices.enroll("gus", &code, "aa", true).unwrap().owner,
            None
        );
        // An account cannot claim an operator's machine by name.
        assert_eq!(
            devices
                .issue_owned_code("acct-a", "gus", [32; 16], OWNED_LIMIT)
                .unwrap_err()
                .code,
            "name_taken"
        );
        assert_eq!(
            devices.revoke_owned("acct-a", "gus").unwrap_err().code,
            "not_found"
        );
    }

    #[test]
    fn a_pending_owned_code_holds_the_name_until_it_is_spent_or_expires() {
        let (_dir, devices) = store();
        let pending = devices
            .issue_owned_code("acct-a", "spare", [40; 16], OWNED_LIMIT)
            .unwrap();
        assert_eq!(
            devices
                .issue_owned_code("acct-b", "spare", [41; 16], OWNED_LIMIT)
                .unwrap_err()
                .code,
            "name_taken"
        );
        // The owner's new code supersedes its own unused one.
        let newer = devices
            .issue_owned_code("acct-a", "spare", [42; 16], OWNED_LIMIT)
            .unwrap();
        assert!(devices.enroll("spare", &pending, "aa", false).is_err());
        // Revoking a name that never joined cancels the owner's pending code.
        assert_eq!(devices.revoke_owned("acct-a", "spare").unwrap(), (false, 1));
        assert!(devices.enroll("spare", &newer, "aa", false).is_err());
        // Once codes expire unspent, the name is free for anyone.
        devices
            .issue_owned_code("acct-a", "spare", [43; 16], OWNED_LIMIT)
            .unwrap();
        devices.expire_codes();
        devices
            .issue_owned_code("acct-b", "spare", [44; 16], OWNED_LIMIT)
            .unwrap();
    }

    #[test]
    fn an_account_holds_at_most_its_limit_and_revoking_frees_a_place() {
        let (_dir, devices) = store();
        let one = devices
            .issue_owned_code("acct-a", "one", [50; 16], 2)
            .unwrap();
        devices.enroll("one", &one, "aa", false).unwrap();
        devices
            .issue_owned_code("acct-a", "two", [51; 16], 2)
            .unwrap();
        // A new code for a name it already holds is not a new machine.
        devices
            .issue_owned_code("acct-a", "two", [52; 16], 2)
            .unwrap();
        devices
            .issue_owned_code("acct-a", "one", [53; 16], 2)
            .unwrap();
        assert_eq!(
            devices
                .issue_owned_code("acct-a", "three", [54; 16], 2)
                .unwrap_err()
                .code,
            "machine_limit"
        );
        devices.revoke_owned("acct-a", "one").unwrap();
        devices
            .issue_owned_code("acct-a", "three", [55; 16], 2)
            .unwrap();
    }

    #[test]
    fn a_store_from_before_owners_gains_the_column_and_keeps_its_machines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.sqlite3");
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(
                "CREATE TABLE devices (name TEXT PRIMARY KEY, public_key TEXT NOT NULL,
                   enrolled_at INTEGER NOT NULL, revoked_at INTEGER);
                 CREATE TABLE enroll_codes (code_hash TEXT PRIMARY KEY, device TEXT NOT NULL,
                   expires_at INTEGER NOT NULL, used_at INTEGER);
                 INSERT INTO devices VALUES ('gold', 'aa', 1, NULL);",
            )
            .unwrap();
        }
        let devices = Devices::open(&path).unwrap();
        let gold = devices.get("gold").unwrap().unwrap();
        assert_eq!((gold.public_key.as_str(), gold.owner), ("aa", None));
        drop(devices);
        // Opening again is a no-op, not a second ALTER.
        Devices::open(&path).unwrap();
    }
}
