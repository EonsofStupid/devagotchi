//! Machines enrolled to reach this hub over the link (plan N20). A device is a name
//! bound to one Ed25519 public key. Binding happens once, with a one-time code the hub
//! operator issues; the code itself is never stored, only its SHA-256.

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

pub struct Devices {
    connection: Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Device {
    pub name: String,
    pub public_key: String,
    pub enrolled_at: u64,
    pub revoked_at: Option<u64>,
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

fn code_hash(code: &str) -> String {
    hex::encode(Sha256::digest(code.trim().as_bytes()))
}

impl Devices {
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
                 );",
            )
            .map_err(store_error)?;
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

    /// Spend `code` for `device` and bind `public_key` to it. Re-enrolling a name with a
    /// fresh code replaces its key and clears a revocation: a new code is an operator act.
    pub fn enroll(&self, device: &str, code: &str, public_key: &str) -> BridgeResult<()> {
        let refused = |message: &str| BridgeError::new("enroll_refused", message);
        let mut connection = self.connection.lock().expect("device lock");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let row: Option<(String, u64, Option<u64>)> = transaction
            .query_row(
                "SELECT device, expires_at, used_at FROM enroll_codes WHERE code_hash = ?1",
                params![code_hash(code)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(store_error)?;
        let now = now_secs();
        match row {
            None => return Err(refused("unknown enrollment code")),
            Some((bound, _, _)) if bound != device => {
                return Err(refused("this code was issued for another device"));
            }
            Some((_, _, Some(_))) => return Err(refused("this code was already used")),
            Some((_, expires, None)) if expires < now => {
                return Err(refused("this code has expired"));
            }
            Some(_) => {}
        }
        // Spend this code and any other unused one for the device: a binding ends them all.
        cancel_codes(&transaction, device, now)?;
        transaction
            .execute(
                "INSERT INTO devices (name, public_key, enrolled_at, revoked_at) VALUES (?1, ?2, ?3, NULL)
                 ON CONFLICT(name) DO UPDATE SET public_key = excluded.public_key,
                   enrolled_at = excluded.enrolled_at, revoked_at = NULL",
                params![device, public_key, now],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)
    }

    pub fn get(&self, device: &str) -> BridgeResult<Option<Device>> {
        let connection = self.connection.lock().expect("device lock");
        connection
            .query_row(
                "SELECT name, public_key, enrolled_at, revoked_at FROM devices WHERE name = ?1",
                params![device],
                |row| {
                    Ok(Device {
                        name: row.get(0)?,
                        public_key: row.get(1)?,
                        enrolled_at: row.get(2)?,
                        revoked_at: row.get(3)?,
                    })
                },
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
            .prepare("SELECT name, public_key, enrolled_at, revoked_at FROM devices ORDER BY name")
            .map_err(store_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(Device {
                    name: row.get(0)?,
                    public_key: row.get(1)?,
                    enrolled_at: row.get(2)?,
                    revoked_at: row.get(3)?,
                })
            })
            .map_err(store_error)?;
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
            devices.enroll("dabo", &code, "aa").unwrap_err().code,
            "enroll_refused"
        );
        devices.enroll("gus", &code, "aa").unwrap();
        assert_eq!(devices.get("gus").unwrap().unwrap().public_key, "aa");
        assert_eq!(
            devices.enroll("gus", &code, "bb").unwrap_err().code,
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
            devices.enroll("gus", &older, "aa").is_err(),
            "a superseded code must not work"
        );
        let spare = devices.issue_code("dabo", [12; 16]).unwrap();
        devices.enroll("gus", &newer, "aa").unwrap();
        // A code for another device is untouched by gus's binding.
        let pending = devices.issue_code("gus", [13; 16]).unwrap();
        assert_eq!(devices.revoke("gus").unwrap(), (true, 1));
        assert!(
            devices.enroll("gus", &pending, "bb").is_err(),
            "revoke must cancel pending codes"
        );
        assert_eq!(devices.get("gus").unwrap().unwrap().public_key, "aa");
        devices.enroll("dabo", &spare, "cc").unwrap();
        // Revoking a name that never enrolled still cancels its codes.
        let orphan = devices.issue_code("solo", [14; 16]).unwrap();
        assert_eq!(devices.revoke("solo").unwrap(), (false, 1));
        assert!(devices.enroll("solo", &orphan, "dd").is_err());
    }

    #[test]
    fn expired_and_unknown_codes_are_refused() {
        let (_dir, devices) = store();
        let code = devices.issue_code("gus", [1; 16]).unwrap();
        devices.expire_codes();
        assert!(devices.enroll("gus", &code, "aa").is_err());
        assert!(devices.enroll("gus", "dvg-nope", "aa").is_err());
        assert!(devices.get("gus").unwrap().is_none());
    }

    #[test]
    fn revoke_sticks_until_a_new_code_re_enrolls() {
        let (_dir, devices) = store();
        let first = devices.issue_code("gus", [2; 16]).unwrap();
        devices.enroll("gus", &first, "aa").unwrap();
        assert!(!devices.is_revoked("gus").unwrap());
        assert_eq!(devices.revoke("gus").unwrap(), (true, 0));
        assert!(devices.is_revoked("gus").unwrap());
        assert_eq!(devices.revoke("gus").unwrap(), (false, 0));
        let second = devices.issue_code("gus", [3; 16]).unwrap();
        devices.enroll("gus", &second, "cc").unwrap();
        assert!(!devices.is_revoked("gus").unwrap());
        assert_eq!(devices.list().unwrap().len(), 1);
        assert!(devices.is_revoked("never-enrolled").unwrap());
    }
}
