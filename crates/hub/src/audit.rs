//! Append-only, hash-chained audit of every routed call. The chain is verified on
//! health checks so a truncated or edited log is visible, not silent.

use std::{path::Path, sync::Mutex};

use devagotchi_contracts::{BridgeError, BridgeResult};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

pub struct Audit {
    connection: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct AuditEntry<'a> {
    pub subject: &'a str,
    pub via: &'a str,
    pub tool: &'a str,
    pub broker: &'a str,
    pub operation_id: &'a str,
    pub outcome: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ChainStatus {
    pub entries: u64,
    pub ok: bool,
    pub head: String,
}

impl Audit {
    pub fn open(path: &Path) -> BridgeResult<Self> {
        let connection = Connection::open(path)
            .map_err(|error| BridgeError::new("audit_open", error.to_string()))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = FULL;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE IF NOT EXISTS audit (
                   seq INTEGER PRIMARY KEY AUTOINCREMENT,
                   at TEXT NOT NULL,
                   subject TEXT NOT NULL,
                   via TEXT NOT NULL,
                   tool TEXT NOT NULL,
                   broker TEXT NOT NULL,
                   operation_id TEXT NOT NULL,
                   outcome TEXT NOT NULL,
                   prev_hash TEXT NOT NULL,
                   hash TEXT NOT NULL
                 );",
            )
            .map_err(|error| BridgeError::new("audit_open", error.to_string()))?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn record(&self, entry: &AuditEntry<'_>) -> BridgeResult<u64> {
        let mut connection = self.connection.lock().expect("audit lock");
        // IMMEDIATE takes the write lock before reading the head, so the hub and an
        // operator command writing at the same moment cannot fork the chain.
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| BridgeError::new("audit_write", error.to_string()))?;
        let at = now_rfc3339();
        let prev: String = transaction
            .query_row(
                "SELECT hash FROM audit ORDER BY seq DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| BridgeError::new("audit_write", error.to_string()))?
            .unwrap_or_else(|| "0".repeat(64));
        let hash = row_hash(&prev, &at, entry);
        transaction
            .execute(
                "INSERT INTO audit (at, subject, via, tool, broker, operation_id, outcome, prev_hash, hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![at, entry.subject, entry.via, entry.tool, entry.broker, entry.operation_id, entry.outcome, prev, hash],
            )
            .map_err(|error| BridgeError::new("audit_write", error.to_string()))?;
        let seq = transaction.last_insert_rowid() as u64;
        transaction
            .commit()
            .map_err(|error| BridgeError::new("audit_write", error.to_string()))?;
        Ok(seq)
    }

    /// Walk the whole chain. Small logs only; a paged walk arrives with the audit UI.
    pub fn verify(&self) -> BridgeResult<ChainStatus> {
        let connection = self.connection.lock().expect("audit lock");
        let mut statement = connection
            .prepare("SELECT at, subject, via, tool, broker, operation_id, outcome, prev_hash, hash FROM audit ORDER BY seq")
            .map_err(|error| BridgeError::new("audit_read", error.to_string()))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .map_err(|error| BridgeError::new("audit_read", error.to_string()))?;
        let mut prev = "0".repeat(64);
        let mut entries = 0u64;
        let mut ok = true;
        for row in rows {
            let (at, subject, via, tool, broker, operation_id, outcome, prev_hash, hash) =
                row.map_err(|error| BridgeError::new("audit_read", error.to_string()))?;
            let entry = AuditEntry {
                subject: &subject,
                via: &via,
                tool: &tool,
                broker: &broker,
                operation_id: &operation_id,
                outcome: &outcome,
            };
            if prev_hash != prev || row_hash(&prev, &at, &entry) != hash {
                ok = false;
            }
            prev = hash;
            entries += 1;
        }
        Ok(ChainStatus {
            entries,
            ok,
            head: prev,
        })
    }

    #[cfg(test)]
    pub(crate) fn recorded(&self) -> Vec<(String, String, String)> {
        let connection = self.connection.lock().expect("audit lock");
        let mut statement = connection
            .prepare("SELECT subject, tool, outcome FROM audit ORDER BY seq")
            .expect("audit read");
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("audit rows");
        rows.map(|row| row.expect("audit row")).collect()
    }
}

fn row_hash(prev: &str, at: &str, entry: &AuditEntry<'_>) -> String {
    let mut hasher = Sha256::new();
    for part in [
        prev,
        at,
        entry.subject,
        entry.via,
        entry.tool,
        entry.broker,
        entry.operation_id,
        entry.outcome,
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    hex::encode(hasher.finalize())
}

fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (hours, minutes, seconds) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil date from days since epoch (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}.{:03}Z",
        now.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_links_rows_and_detects_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let audit = Audit::open(&dir.path().join("audit.sqlite3")).unwrap();
        for i in 0..3 {
            let id = format!("op-{i}");
            audit
                .record(&AuditEntry {
                    subject: "jesse",
                    via: "dev",
                    tool: "operation_start",
                    broker: "gold-ws",
                    operation_id: &id,
                    outcome: "ok",
                })
                .unwrap();
        }
        let status = audit.verify().unwrap();
        assert_eq!(status.entries, 3);
        assert!(status.ok);
        {
            let connection = audit.connection.lock().unwrap();
            connection
                .execute("UPDATE audit SET outcome = 'denied' WHERE seq = 2", [])
                .unwrap();
        }
        let tampered = audit.verify().unwrap();
        assert_eq!(tampered.entries, 3);
        assert!(!tampered.ok);
    }

    #[test]
    fn timestamp_is_rfc3339_utc() {
        let ts = now_rfc3339();
        assert_eq!(ts.len(), 24);
        assert!(ts.ends_with('Z') && ts.chars().nth(10) == Some('T'));
    }
}
