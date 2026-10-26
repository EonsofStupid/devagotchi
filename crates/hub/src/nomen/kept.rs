//! Sign-ins Nomen confirmed, kept on disk so a hub restart during a Nomen outage does not
//! sign everyone out (increment 27b). A row holds the SHA-256 of the token, never the
//! token, with the account it proved and until when it may be honoured: the token's own
//! expiry, at most 12 hours after Nomen last confirmed it. The hub reads the rows only
//! while Nomen cannot be reached; when Nomen answers, its answer decides, and an answer
//! that the token is no longer good deletes the row. A store that fails is logged and
//! ignored: sign-in never depends on this file.

use std::{path::Path, sync::Mutex};

use devagotchi_contracts::{BridgeError, BridgeResult};
use rusqlite::{Connection, params};

use crate::auth::Subject;

/// One confirmed sign-in, as kept.
pub(super) struct Kept {
    pub digest: [u8; 32],
    pub subject: Subject,
    /// Unix seconds Nomen last confirmed it, and until when it may be honoured.
    pub confirmed_at: u64,
    pub honour_until: u64,
}

pub(super) struct SignIns {
    connection: Mutex<Connection>,
}

fn store_error(error: rusqlite::Error) -> BridgeError {
    BridgeError::new("sign_in_store", error.to_string())
}

impl SignIns {
    pub fn open(path: &Path) -> BridgeResult<Self> {
        let connection = Connection::open(path).map_err(store_error)?;
        // Losing the last write to a power cut costs one re-check with Nomen, nothing more.
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA busy_timeout = 2000;
                 CREATE TABLE IF NOT EXISTS confirmed (
                   digest BLOB PRIMARY KEY,
                   subject TEXT NOT NULL,
                   name TEXT,
                   org TEXT,
                   confirmed_at INTEGER NOT NULL,
                   honour_until INTEGER NOT NULL
                 );",
            )
            .map_err(store_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Every sign-in still honourable at `now`; lapsed rows are deleted first.
    pub fn load(&self, now: u64) -> BridgeResult<Vec<Kept>> {
        let connection = self.connection.lock().expect("sign-ins");
        connection
            .execute(
                "DELETE FROM confirmed WHERE honour_until <= ?1",
                params![now as i64],
            )
            .map_err(store_error)?;
        let mut statement = connection
            .prepare("SELECT digest, subject, name, org, confirmed_at, honour_until FROM confirmed")
            .map_err(store_error)?;
        let rows = statement
            .query_map([], |row| {
                let digest: Vec<u8> = row.get(0)?;
                Ok((
                    digest,
                    Subject {
                        id: row.get(1)?,
                        via: "nomen",
                        name: row.get(2)?,
                        org: row.get(3)?,
                    },
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(store_error)?;
        let mut kept = Vec::new();
        for row in rows {
            let (digest, subject, confirmed_at, honour_until) = row.map_err(store_error)?;
            if let Ok(digest) = <[u8; 32]>::try_from(digest.as_slice()) {
                kept.push(Kept {
                    digest,
                    subject,
                    confirmed_at: confirmed_at.max(0) as u64,
                    honour_until: honour_until.max(0) as u64,
                });
            }
        }
        Ok(kept)
    }

    /// Keep (or refresh) a confirmation; lapsed rows go at the same time.
    pub fn keep(&self, kept: &Kept) -> BridgeResult<()> {
        let connection = self.connection.lock().expect("sign-ins");
        connection
            .execute(
                "INSERT INTO confirmed (digest, subject, name, org, confirmed_at, honour_until)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(digest) DO UPDATE SET subject = excluded.subject, name = excluded.name,
                   org = excluded.org, confirmed_at = excluded.confirmed_at,
                   honour_until = excluded.honour_until",
                params![
                    kept.digest.as_slice(),
                    kept.subject.id,
                    kept.subject.name,
                    kept.subject.org,
                    kept.confirmed_at as i64,
                    kept.honour_until as i64
                ],
            )
            .map_err(store_error)?;
        connection
            .execute(
                "DELETE FROM confirmed WHERE honour_until <= ?1",
                params![kept.confirmed_at as i64],
            )
            .map_err(store_error)?;
        Ok(())
    }

    /// Nomen said this token is no longer good: never honour it again.
    pub fn forget(&self, digest: &[u8; 32]) -> BridgeResult<()> {
        self.connection
            .lock()
            .expect("sign-ins")
            .execute(
                "DELETE FROM confirmed WHERE digest = ?1",
                params![digest.as_slice()],
            )
            .map_err(store_error)?;
        Ok(())
    }
}
