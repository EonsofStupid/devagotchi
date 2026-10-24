//! Browser sessions that outlast a restart, so a phone that turned on notifications
//! stays signed in and a push opens straight onto Approve.
//!
//! - A row is keyed by the SHA-256 of the browser's random session id. The browser holds
//!   the id; the hub never stores it.
//! - Nomen's tokens are sealed with AES-256-GCM under `session.key` (0600, made on first
//!   use, beside the store) and bound to the row's key. The database on its own gives
//!   away no token, and a sealed blob moved to another row does not open.
//! - A session ends when its person signs out, after [`IDLE`] unused, at [`MAX`] however
//!   much it is used, or as soon as Nomen refuses to refresh it (`web.rs`).
//! - Times are Unix milliseconds.

use std::{path::Path, sync::Mutex};

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use devagotchi_contracts::{BridgeError, BridgeResult};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// A session nobody has used for this long ends.
pub const IDLE: u64 = 30 * 24 * 3600 * 1000;
/// No session lasts longer than this, however often it is used.
pub const MAX: u64 = 90 * 24 * 3600 * 1000;
const MAX_ROWS: i64 = 10_000;
const KEY_FILE: &str = "session.key";
const STORE_FILE: &str = "sessions.sqlite3";

/// What the hub holds for a session: Nomen's access token and, when Nomen gave one, the
/// refresh token that renews it.
#[derive(Clone, Serialize, Deserialize)]
pub struct Held {
    pub access: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
}

pub struct Stored {
    pub held: Held,
    /// When the access token stops being accepted.
    pub access_until: u64,
    pub last_seen: u64,
}

pub struct SessionStore {
    connection: Mutex<Connection>,
    key: LessSafeKey,
}

fn store_error(error: rusqlite::Error) -> BridgeError {
    BridgeError::new("session_store", error.to_string())
}

pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

impl SessionStore {
    /// The store in `state_dir`, with its sealing key; both are made on first use, 0600.
    pub fn open(state_dir: &Path) -> BridgeResult<Self> {
        let key_path = state_dir.join(KEY_FILE);
        let key = match std::fs::read(&key_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let bytes = crate::link::random::<32>().to_vec();
                crate::push::write_private(&key_path, &bytes)?;
                bytes
            }
            Err(error) => {
                return Err(BridgeError::new(
                    "state_file",
                    format!("{}: {error}", key_path.display()),
                ));
            }
        };
        let key = UnboundKey::new(&AES_256_GCM, &key).map_err(|_| {
            BridgeError::new(
                "state_file",
                format!("{} is not a 32-byte key", key_path.display()),
            )
        })?;
        let path = state_dir.join(STORE_FILE);
        if !path.exists() {
            // SQLite gives its -wal and -shm files the main file's mode, so all three are 0600.
            crate::push::write_private(&path, &[])?;
        }
        let connection = Connection::open(&path).map_err(store_error)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = FULL;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE IF NOT EXISTS web_sessions (
                   id_hash BLOB PRIMARY KEY,
                   sealed BLOB NOT NULL,
                   access_until INTEGER NOT NULL,
                   created_at INTEGER NOT NULL,
                   last_seen INTEGER NOT NULL,
                   expires_at INTEGER NOT NULL
                 );",
            )
            .map_err(store_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
            key: LessSafeKey::new(key),
        })
    }

    fn seal(&self, id_hash: &[u8; 32], held: &Held) -> BridgeResult<Vec<u8>> {
        let nonce = crate::link::random::<NONCE_LEN>();
        let mut body = serde_json::to_vec(held).expect("tokens serialize");
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(id_hash),
                &mut body,
            )
            .map_err(|_| BridgeError::new("session_store", "could not seal a session"))?;
        let mut sealed = nonce.to_vec();
        sealed.extend_from_slice(&body);
        Ok(sealed)
    }

    fn unseal(&self, id_hash: &[u8; 32], sealed: &[u8]) -> Option<Held> {
        if sealed.len() <= NONCE_LEN {
            return None;
        }
        let (nonce, body) = sealed.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
        let mut body = body.to_vec();
        let plain = self
            .key
            .open_in_place(nonce, Aad::from(id_hash), &mut body)
            .ok()?;
        serde_json::from_slice(plain).ok()
    }

    /// Start a session that ends at `expires_at` at the latest. Ended sessions are swept
    /// first, and past [`MAX_ROWS`] the least recently used one makes room.
    pub fn put(
        &self,
        id_hash: &[u8; 32],
        held: &Held,
        access_until: u64,
        expires_at: u64,
        now: u64,
    ) -> BridgeResult<()> {
        let sealed = self.seal(id_hash, held)?;
        let connection = self.connection.lock().expect("session store");
        connection
            .execute(
                "DELETE FROM web_sessions WHERE expires_at <= ?1 OR last_seen + ?2 <= ?1",
                params![now as i64, IDLE as i64],
            )
            .map_err(store_error)?;
        connection
            .execute(
                "DELETE FROM web_sessions WHERE id_hash IN (SELECT id_hash FROM web_sessions
                   ORDER BY last_seen LIMIT max(0, (SELECT count(*) FROM web_sessions) - ?1 + 1))",
                params![MAX_ROWS],
            )
            .map_err(store_error)?;
        connection
            .execute(
                "INSERT OR REPLACE INTO web_sessions
                   (id_hash, sealed, access_until, created_at, last_seen, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                params![
                    &id_hash[..],
                    sealed,
                    access_until as i64,
                    now as i64,
                    expires_at as i64
                ],
            )
            .map_err(store_error)?;
        Ok(())
    }

    /// The session under `id_hash` while it lasts. One that ended, or whose seal does
    /// not open, is removed and reads as none.
    pub fn get(&self, id_hash: &[u8; 32], now: u64) -> BridgeResult<Option<Stored>> {
        let connection = self.connection.lock().expect("session store");
        let row: Option<(Vec<u8>, i64, i64, i64)> = connection
            .query_row(
                "SELECT sealed, access_until, last_seen, expires_at FROM web_sessions
                 WHERE id_hash = ?1",
                params![&id_hash[..]],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(store_error)?;
        let Some((sealed, access_until, last_seen, expires_at)) = row else {
            return Ok(None);
        };
        let (access_until, last_seen, expires_at) =
            (access_until as u64, last_seen as u64, expires_at as u64);
        let ended = expires_at <= now || last_seen + IDLE <= now;
        let held = if ended {
            None
        } else {
            self.unseal(id_hash, &sealed)
        };
        let Some(held) = held else {
            connection
                .execute(
                    "DELETE FROM web_sessions WHERE id_hash = ?1",
                    params![&id_hash[..]],
                )
                .map_err(store_error)?;
            return Ok(None);
        };
        Ok(Some(Stored {
            held,
            access_until,
            last_seen,
        }))
    }

    /// Record a use, which keeps the session from going idle.
    pub fn touch(&self, id_hash: &[u8; 32], now: u64) -> BridgeResult<()> {
        self.connection
            .lock()
            .expect("session store")
            .execute(
                "UPDATE web_sessions SET last_seen = ?2 WHERE id_hash = ?1",
                params![&id_hash[..], now as i64],
            )
            .map_err(store_error)?;
        Ok(())
    }

    /// Keep the tokens a refresh returned (Nomen rotates the refresh token).
    pub fn renew(
        &self,
        id_hash: &[u8; 32],
        held: &Held,
        access_until: u64,
        now: u64,
    ) -> BridgeResult<()> {
        let sealed = self.seal(id_hash, held)?;
        self.connection
            .lock()
            .expect("session store")
            .execute(
                "UPDATE web_sessions SET sealed = ?2, access_until = ?3, last_seen = ?4
                 WHERE id_hash = ?1",
                params![&id_hash[..], sealed, access_until as i64, now as i64],
            )
            .map_err(store_error)?;
        Ok(())
    }

    /// End the session; returns what it held, so the refresh token can be revoked.
    pub fn remove(&self, id_hash: &[u8; 32]) -> Option<Held> {
        let connection = self.connection.lock().expect("session store");
        let sealed: Option<Vec<u8>> = connection
            .query_row(
                "SELECT sealed FROM web_sessions WHERE id_hash = ?1",
                params![&id_hash[..]],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();
        let _ = connection.execute(
            "DELETE FROM web_sessions WHERE id_hash = ?1",
            params![&id_hash[..]],
        );
        sealed.and_then(|sealed| self.unseal(id_hash, &sealed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(access: &str, refresh: Option<&str>) -> Held {
        Held {
            access: access.into(),
            refresh: refresh.map(str::to_string),
        }
    }

    #[test]
    fn a_session_survives_reopening_the_store_and_ends_idle_or_at_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let t = 1_000_000_000;
        {
            let store = SessionStore::open(dir.path()).unwrap();
            store
                .put(&a, &held("acc-a", Some("ref-a")), t + 1000, t + MAX, t)
                .unwrap();
            store
                .put(&b, &held("acc-b", None), t + 1000, t + MAX, t)
                .unwrap();
            store
                .put(&c, &held("acc-c", Some("ref-c")), t + 1000, t + 5000, t)
                .unwrap();
        }
        // A restart: a new store over the same directory reads the same sessions.
        let store = SessionStore::open(dir.path()).unwrap();
        let got = store.get(&a, t + 10).unwrap().unwrap();
        assert_eq!(got.held.access, "acc-a");
        assert_eq!(got.held.refresh.as_deref(), Some("ref-a"));
        assert_eq!(got.access_until, t + 1000);

        assert!(store.get(&c, t + 5000).unwrap().is_none(), "past its limit");
        assert!(
            store.get(&c, t + 10).unwrap().is_none(),
            "and gone for good"
        );

        store.touch(&a, t + IDLE - 1).unwrap();
        assert!(
            store.get(&a, t + IDLE + 10).unwrap().is_some(),
            "used, so not idle"
        );
        assert!(
            store.get(&b, t + IDLE).unwrap().is_none(),
            "unused for IDLE ends"
        );

        store
            .renew(
                &a,
                &held("acc-a2", Some("ref-a2")),
                t + IDLE + 5000,
                t + IDLE + 20,
            )
            .unwrap();
        let renewed = store.get(&a, t + IDLE + 30).unwrap().unwrap();
        assert_eq!(renewed.held.access, "acc-a2");
        assert_eq!(renewed.held.refresh.as_deref(), Some("ref-a2"));
        let removed = store.remove(&a).unwrap();
        assert_eq!(removed.refresh.as_deref(), Some("ref-a2"));
        assert!(store.get(&a, t + IDLE + 40).unwrap().is_none());
    }

    #[test]
    fn tokens_are_sealed_at_rest_and_bound_to_their_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let (a, b) = ([7u8; 32], [8u8; 32]);
        let t = now_millis();
        store
            .put(
                &a,
                &held("ACCESS-SECRET-A", Some("REFRESH-SECRET-A")),
                t + 60_000,
                t + MAX,
                t,
            )
            .unwrap();
        store
            .put(&b, &held("ACCESS-SECRET-B", None), t + 60_000, t + MAX, t)
            .unwrap();
        for name in [STORE_FILE, "sessions.sqlite3-wal"] {
            let bytes = std::fs::read(dir.path().join(name)).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains("SECRET"),
                "{name} holds a token in the clear"
            );
        }
        // Move a's sealed blob onto b's row: it does not open there, and b is dropped.
        {
            let connection = store.connection.lock().unwrap();
            connection
                .execute(
                    "UPDATE web_sessions SET sealed =
                       (SELECT sealed FROM web_sessions WHERE id_hash = ?1) WHERE id_hash = ?2",
                    params![&a[..], &b[..]],
                )
                .unwrap();
        }
        assert!(store.get(&b, t + 1).unwrap().is_none());
        assert_eq!(
            store.get(&a, t + 1).unwrap().unwrap().held.access,
            "ACCESS-SECRET-A"
        );

        // Another key cannot open the store's sessions.
        std::fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
        crate::push::write_private(&dir.path().join(KEY_FILE), &[9u8; 32]).unwrap();
        drop(store);
        let rekeyed = SessionStore::open(dir.path()).unwrap();
        assert!(rekeyed.get(&a, t + 1).unwrap().is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for name in [KEY_FILE, STORE_FILE] {
                let mode = std::fs::metadata(dir.path().join(name))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600, "{name}");
            }
        }
    }
}
